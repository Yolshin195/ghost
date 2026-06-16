/// Консольный клиент для записи аудио и отправки на сервер.
///
/// Использование:
///   cargo run --bin recorder -- --server wss://192.168.1.10:3005
///   cargo run --bin recorder -- --list-devices
///   cargo run --bin recorder -- --device 2 --server wss://...
///
/// Управление во время записи:
///   Enter         — старт / стоп
///   q + Enter     — выход

use std::{
    io::{self, BufRead, Write},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use anyhow::{Context, Result};
use clap::Parser;
use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    StreamConfig,
};
use tokio::sync::mpsc;
use tokio_tungstenite::{
    connect_async_tls_with_config,
    tungstenite::protocol::Message,
    Connector,
};
use futures_util::{SinkExt, StreamExt};
use rustls::{ClientConfig, RootCertStore};
use std::sync::Arc as StdArc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, Error as RustlsError, SignatureScheme};

// ─── CLI ─────────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(
    name    = "recorder",
    about   = "Консольный клиент записи аудио → WebSocket сервер",
    version
)]
struct Cli {
    /// Адрес сервера (wss://host:port)
    #[arg(short, long, default_value = "wss://127.0.0.1:3005")]
    server: String,

    /// Индекс устройства ввода (см. --list-devices)
    #[arg(short, long)]
    device: Option<usize>,

    /// Показать список доступных устройств и выйти
    #[arg(short, long)]
    list_devices: bool,

    /// Частота дискретизации (Гц)
    #[arg(long, default_value_t = 16000)]
    sample_rate: u32,

    /// Число каналов (1 = моно, 2 = стерео)
    #[arg(long, default_value_t = 1)]
    channels: u16,

    /// Усиление сигнала (1.0 = без изменений)
    #[arg(long, default_value_t = 1.0)]
    gain: f32,

    /// Порог шумового гейта в дБ (−80 = отключён)
    #[arg(long, default_value_t = -80.0)]
    gate_db: f32,

    /// Принять самоподписанный TLS-сертификат (небезопасно, только для локальной сети)
    #[arg(long)]
    insecure: bool,
}

// ─── Утилиты ─────────────────────────────────────────────────────────────────

fn list_input_devices() -> Result<()> {
    let host = cpal::default_host();
    println!("\n{:<4} {}", "IDX", "Устройство");
    println!("{}", "─".repeat(60));

    let default_name = host
        .default_input_device()
        .and_then(|d| d.name().ok())
        .unwrap_or_default();

    for (i, dev) in host.input_devices()?.enumerate() {
        let name = dev.name().unwrap_or_else(|_| "<?>".into());
        let mark = if name == default_name { " ◀ по умолчанию" } else { "" };
        println!("{:<4} {}{}", i, name, mark);
    }
    println!();
    Ok(())
}

fn pick_device(idx: Option<usize>) -> Result<cpal::Device> {
    let host = cpal::default_host();
    match idx {
        None => host
            .default_input_device()
            .context("Нет устройства ввода по умолчанию"),
        Some(i) => host
            .input_devices()?
            .nth(i)
            .with_context(|| format!("Устройства с индексом {i} не существует")),
    }
}

/// Строит WAV-заголовок для PCM f32le.
/// Сервер получает полноценный .wav файл, который ffmpeg/whisper может открыть.
#[allow(dead_code)]
fn wav_header(sample_rate: u32, channels: u16, num_samples: u32) -> Vec<u8> {
    let byte_rate   = sample_rate * channels as u32 * 4; // f32 = 4 bytes
    let data_size   = num_samples * channels as u32 * 4;
    let chunk_size  = 36 + data_size;

    let mut h = Vec::with_capacity(44);
    h.extend_from_slice(b"RIFF");
    h.extend_from_slice(&chunk_size.to_le_bytes());
    h.extend_from_slice(b"WAVE");
    h.extend_from_slice(b"fmt ");
    h.extend_from_slice(&16u32.to_le_bytes());          // subchunk size
    h.extend_from_slice(&3u16.to_le_bytes());           // PCM float = 3
    h.extend_from_slice(&channels.to_le_bytes());
    h.extend_from_slice(&sample_rate.to_le_bytes());
    h.extend_from_slice(&byte_rate.to_le_bytes());
    h.extend_from_slice(&(channels * 4).to_le_bytes()); // block align
    h.extend_from_slice(&32u16.to_le_bytes());          // bits per sample
    h.extend_from_slice(b"data");
    h.extend_from_slice(&data_size.to_le_bytes());
    h
}

// ─── TLS ─────────────────────────────────────────────────────────────────────

// Верификатор, который принимает любой сертификат (для --insecure / локальной сети)
#[derive(Debug)]
struct NoVerifier;

impl ServerCertVerifier for NoVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, RustlsError> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![
            SignatureScheme::RSA_PKCS1_SHA256,
            SignatureScheme::RSA_PKCS1_SHA384,
            SignatureScheme::RSA_PKCS1_SHA512,
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::ECDSA_NISTP384_SHA384,
            SignatureScheme::ECDSA_NISTP521_SHA512,
            SignatureScheme::RSA_PSS_SHA256,
            SignatureScheme::RSA_PSS_SHA384,
            SignatureScheme::RSA_PSS_SHA512,
            SignatureScheme::ED25519,
        ]
    }
}

fn make_tls_connector(insecure: bool) -> Result<Connector> {
    if insecure {
        // Используем rustls с отключённой верификацией — совместимо с сервером на rustls.
        // native-tls (SecureTransport на macOS 13+) не договаривается с rustls по TLS 1.3.
        let cfg = ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(StdArc::new(NoVerifier))
            .with_no_client_auth();
        Ok(Connector::Rustls(StdArc::new(cfg)))
    } else {
        let mut roots = RootCertStore::empty();
        for cert in rustls_native_certs::load_native_certs()? {
            roots.add(cert).ok();
        }
        let cfg = ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        Ok(Connector::Rustls(StdArc::new(cfg)))
    }
}

// ─── VU-метр в терминале ─────────────────────────────────────────────────────

fn draw_vu(rms: f32) {
    let db = if rms > 0.0 { 20.0 * rms.log10() } else { -f32::INFINITY };
    let pct = ((db + 60.0) / 60.0).clamp(0.0, 1.0);
    let width = 30usize;
    let filled = (pct * width as f32) as usize;

    let bar: String = (0..width)
        .map(|i| if i < filled { '█' } else { '░' })
        .collect();

    let db_str = if db.is_finite() {
        format!("{:+.1} дБ", db)
    } else {
        "  −∞  ".to_string()
    };

    // \r — возврат в начало строки без перевода (перезаписываем на месте)
    print!("\r  [{}] {}   ", bar, db_str);
    io::stdout().flush().ok();
}

// ─── Основная логика записи ───────────────────────────────────────────────────

async fn run_session(
    device: &cpal::Device,
    config: StreamConfig,
    sample_rate: u32,
    channels: u16,
    gain: f32,
    gate_db: f32,
    server_url: &str,
    insecure: bool,
    stop_signal: Arc<AtomicBool>,
) -> Result<()> {
    // Канал для передачи аудио-сэмплов из колбэка CPAL в async-код
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<f32>>();

    // Флаг активной записи (используется внутри CPAL-колбэка)
    let recording = Arc::new(AtomicBool::new(true));
    let rec_flag  = recording.clone();

    let gate_linear: f32 = if gate_db <= -80.0 {
        0.0
    } else {
        10f32.powf(gate_db / 20.0)
    };

    // ── CPAL stream ──
    let stream = device.build_input_stream(
        &config,
        move |data: &[f32], _| {
            if !rec_flag.load(Ordering::Relaxed) { return; }

            // RMS для гейта
            let rms: f32 = {
                let sum: f32 = data.iter().map(|s| s * s).sum();
                (sum / data.len() as f32).sqrt()
            };

            let samples: Vec<f32> = if gate_linear > 0.0 && rms < gate_linear {
                vec![0.0f32; data.len()]
            } else {
                data.iter().map(|&s| (s * gain).clamp(-1.0, 1.0)).collect()
            };

            tx.send(samples).ok();
        },
        |e| eprintln!("\nОшибка аудио: {e}"),
        None,
    )?;
    stream.play()?;

    // ── WebSocket ──
    // ── WebSocket ──
    // Автоматически используем insecure для локальных адресов
    let use_insecure = insecure || server_url.contains("127.0.0.1") || server_url.contains("localhost");
    let tls = make_tls_connector(use_insecure)?;
    let url = format!("{}/ws/audio-pcm?sr={}&ch={}", server_url, sample_rate, channels);

    println!("  Подключение к {} ...", url);
    if use_insecure && !insecure {
        println!("  ⚠️  Локальный адрес — используем insecure TLS режим");
    }
    let (ws_stream, _) = connect_async_tls_with_config(
        &url,
        None,
        false,
        Some(tls),
    )
    .await
    .context("Не удалось подключиться к серверу")?;

    let (mut ws_tx, mut ws_rx) = ws_stream.split();
    println!("  Соединение установлено. Идёт запись...\n");

    // Задача — слушать входящие сообщения от сервера (file_saved и т.д.)
    let srv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = ws_rx.next().await {
            if let Message::Text(txt) = msg {
                // Парсим минимально
                if txt.contains("file_saved") {
                    // {"type":"file_saved","filename":"...","size_kb":123}
                    let fname = txt
                        .split("\"filename\":\"").nth(1)
                        .and_then(|s| s.split('"').next())
                        .unwrap_or("?");
                    let size = txt
                        .split("\"size_kb\":").nth(1)
                        .and_then(|s| s.split(['}', ',']).next())
                        .unwrap_or("?");
                    println!("\n  ✓ Сохранён сегмент: {} ({} KB)", fname, size);
                }
            }
        }
    });

    // Накапливаем сэмплы, раз в 250 мс шлём пакет (аналогично MediaRecorder)
    const FLUSH_MS: u64 = 250;
    let samples_per_flush = (sample_rate as u64 * channels as u64 * FLUSH_MS / 1000) as usize;
    let mut accumulator: Vec<f32> = Vec::with_capacity(samples_per_flush * 2);

    loop {
        // Проверяем сигнал остановки
        if stop_signal.load(Ordering::Relaxed) {
            break;
        }

        // Таймаут = 100 мс для быстрой реакции на сигнал остановки
        match tokio::time::timeout(Duration::from_millis(100), rx.recv()).await {
            Ok(Some(chunk)) => {
                // Обновляем VU-метр по последнему чанку
                let rms: f32 = {
                    let sum: f32 = chunk.iter().map(|s| s * s).sum();
                    (sum / chunk.len() as f32).sqrt()
                };
                draw_vu(rms);

                accumulator.extend_from_slice(&chunk);

                if accumulator.len() >= samples_per_flush {
                    // Конвертируем f32 сэмплы в байты (little-endian)
                    let bytes: Vec<u8> = accumulator
                        .iter()
                        .flat_map(|s| s.to_le_bytes())
                        .collect();
                    accumulator.clear();

                    if ws_tx.send(Message::Binary(bytes)).await.is_err() {
                        println!("\n  Соединение с сервером закрыто.");
                        break;
                    }
                }
            }
            Ok(None) | Err(_) => {
                // Канал закрыт или таймаут — продолжаем ждать сигнала остановки
                if stop_signal.load(Ordering::Relaxed) {
                    break;
                }
            }
        }
    }

    // Сбрасываем остаток буфера
    if !accumulator.is_empty() {
        let bytes: Vec<u8> = accumulator
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        ws_tx.send(Message::Binary(bytes)).await.ok();
    }

    ws_tx.send(Message::Close(None)).await.ok();
    srv_task.abort();
    drop(stream);

    Ok(())
}

// ─── main ─────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    if cli.list_devices {
        return list_input_devices();
    }

    println!(r#"
 ╔══════════════════════════════════════╗
 ║   Audio Recorder  →  WebSocket      ║
 ╚══════════════════════════════════════╝
"#);

    let device = pick_device(cli.device)?;
    println!("  Устройство : {}", device.name().unwrap_or_default());
    println!("  Сервер     : {}", cli.server);
    println!("  Частота    : {} Гц, {} кан.", cli.sample_rate, cli.channels);
    println!("  Gain       : {:.1}×   Gate: {} дБ",
        cli.gain,
        if cli.gate_db <= -80.0 { "отключён".to_string() } else { cli.gate_db.to_string() }
    );
    if cli.insecure {
        println!("  ⚠  TLS: принимаем любой сертификат (--insecure)");
    }
    println!();
    println!("  Нажмите Enter — начать запись");
    println!("  Во время записи Enter — остановить");
    println!("  q + Enter — выход\n");

    // Используем дефолтную конфигурацию устройства
    let config = device
        .default_input_config()
        .context("Не удалось получить конфигурацию устройства")?
        .config();
    
    println!("  ✓ Конфигурация: {} Гц, {} кан.", config.sample_rate.0, config.channels);

    // ── главный цикл: Enter = старт/стоп ──
    let stdin = io::stdin();
    let mut recording = false;
    let mut recording_thread: Option<std::thread::JoinHandle<()>> = None;
    let mut stop_signal: Option<Arc<AtomicBool>> = None;

    for line in stdin.lock().lines() {
        let line = line.unwrap_or_default();
        let trimmed = line.trim().to_lowercase();

        if trimmed == "q" || trimmed == "quit" || trimmed == "exit" {
            if recording {
                if let Some(signal) = &stop_signal {
                    signal.store(true, Ordering::Relaxed);
                }
                if let Some(thread) = recording_thread.take() {
                    let _ = thread.join();
                }
                std::thread::sleep(Duration::from_millis(500));
            }
            println!("\n  До свидания.");
            break;
        }

        if !recording {
            // ── Старт ──
            recording = true;
            let stop = Arc::new(AtomicBool::new(false));
            stop_signal = Some(stop.clone());

            let device_name = device.name().unwrap_or_default();
            let host = cpal::default_host();
            let dev = host
                .input_devices()?
                .find(|d| d.name().ok().as_deref() == Some(&device_name))
                .or_else(|| host.default_input_device())
                .context("Устройство не найдено")?;

            let cfg    = config.clone();
            let sr     = config.sample_rate.0;
            let ch     = config.channels;
            let gain   = cli.gain;
            let gate   = cli.gate_db;
            let server = cli.server.clone();
            let ins    = cli.insecure;

            let thread = std::thread::spawn(move || {
                let rt = tokio::runtime::Runtime::new().unwrap();
                rt.block_on(async {
                    match run_session(&dev, cfg, sr, ch, gain, gate, &server, ins, stop).await {
                        Ok(_) => {},
                        Err(e) => eprintln!("\n  Ошибка записи: {e:#}"),
                    }
                });
                println!("\n  Запись остановлена. Нажмите Enter чтобы начать снова.");
            });

            recording_thread = Some(thread);
            println!("  ● Запись началась... (Enter — стоп)\n");
        } else {
            // ── Стоп ──
            recording = false;
            if let Some(signal) = &stop_signal {
                signal.store(true, Ordering::Relaxed);
            }
            if let Some(thread) = recording_thread.take() {
                let _ = thread.join();
            }
            std::thread::sleep(Duration::from_millis(200));
            println!("\n  ■ Остановлено. Нажмите Enter чтобы записать снова.\n");
        }
    }

    Ok(())
}