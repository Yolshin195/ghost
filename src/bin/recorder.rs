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
    #[arg(short, long, default_value = "wss://127.0.0.1:3005")]
    server: String,

    #[arg(short, long)]
    device: Option<usize>,

    #[arg(short, long)]
    list_devices: bool,

    #[arg(long, default_value_t = 16000)]
    sample_rate: u32,

    #[arg(long, default_value_t = 1)]
    channels: u16,

    #[arg(long, default_value_t = 1.0)]
    gain: f32,

    #[arg(long, default_value_t = -80.0)]
    gate_db: f32,

    #[arg(long)]
    insecure: bool,
}

// ─── Утилиты ─────────────────────────────────────────────────────────────────

// cpal 0.18: name() удалён, используем description().name()
fn device_name(device: &cpal::Device) -> String {
    device
        .description()
        .map(|d| d.name().to_string())
        .unwrap_or_else(|_| "<?>".to_string())
}

fn list_input_devices() -> Result<()> {
    let host = cpal::default_host();
    println!("\n{:<4} {}", "IDX", "Устройство");
    println!("{}", "─".repeat(60));

    let default_name = host
        .default_input_device()
        .map(|d| device_name(&d))
        .unwrap_or_default();

    for (i, dev) in host.input_devices()?.enumerate() {
        let name = device_name(&dev);
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

#[allow(dead_code)]
fn wav_header(sample_rate: u32, channels: u16, num_samples: u32) -> Vec<u8> {
    let byte_rate  = sample_rate * channels as u32 * 4;
    let data_size  = num_samples * channels as u32 * 4;
    let chunk_size = 36 + data_size;

    let mut h = Vec::with_capacity(44);
    h.extend_from_slice(b"RIFF");
    h.extend_from_slice(&chunk_size.to_le_bytes());
    h.extend_from_slice(b"WAVE");
    h.extend_from_slice(b"fmt ");
    h.extend_from_slice(&16u32.to_le_bytes());
    h.extend_from_slice(&3u16.to_le_bytes());
    h.extend_from_slice(&channels.to_le_bytes());
    h.extend_from_slice(&sample_rate.to_le_bytes());
    h.extend_from_slice(&byte_rate.to_le_bytes());
    h.extend_from_slice(&(channels * 4).to_le_bytes());
    h.extend_from_slice(&32u16.to_le_bytes());
    h.extend_from_slice(b"data");
    h.extend_from_slice(&data_size.to_le_bytes());
    h
}

// ─── TLS ─────────────────────────────────────────────────────────────────────

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
        let cfg = ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(StdArc::new(NoVerifier))
            .with_no_client_auth();
        Ok(Connector::Rustls(StdArc::new(cfg)))
    } else {
        let mut roots = RootCertStore::empty();
        let loaded = rustls_native_certs::load_native_certs();
        for cert in loaded.certs {
            roots.add(cert).ok();
        }
        let cfg = ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        Ok(Connector::Rustls(StdArc::new(cfg)))
    }
}

// ─── VU-метр ─────────────────────────────────────────────────────────────────

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
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<f32>>();

    let recording = Arc::new(AtomicBool::new(true));
    let rec_flag  = recording.clone();

    let gate_linear: f32 = if gate_db <= -80.0 {
        0.0
    } else {
        10f32.powf(gate_db / 20.0)
    };

    // cpal 0.18: build_input_stream принимает StreamConfig по значению (без &)
    let stream = device.build_input_stream(
        config,
        move |data: &[f32], _| {
            if !rec_flag.load(Ordering::Relaxed) { return; }

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
    // cpal 0.18: stream создаётся на паузе на ВСЕХ платформах — play() обязателен
    stream.play()?;

    let use_insecure = insecure
        || server_url.contains("127.0.0.1")
        || server_url.contains("localhost");
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

    let srv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = ws_rx.next().await {
            if let Message::Text(txt) = msg {
                if txt.contains("file_saved") {
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

    const FLUSH_MS: u64 = 250;
    let samples_per_flush = (sample_rate as u64 * channels as u64 * FLUSH_MS / 1000) as usize;
    let mut accumulator: Vec<f32> = Vec::with_capacity(samples_per_flush * 2);

    loop {
        if stop_signal.load(Ordering::Relaxed) {
            break;
        }

        match tokio::time::timeout(Duration::from_millis(100), rx.recv()).await {
            Ok(Some(chunk)) => {
                let rms: f32 = {
                    let sum: f32 = chunk.iter().map(|s| s * s).sum();
                    (sum / chunk.len() as f32).sqrt()
                };
                draw_vu(rms);

                accumulator.extend_from_slice(&chunk);

                if accumulator.len() >= samples_per_flush {
                    let bytes: Vec<u8> = accumulator
                        .iter()
                        .flat_map(|s| s.to_le_bytes())
                        .collect();
                    accumulator.clear();

                    if ws_tx.send(Message::Binary(bytes.into())).await.is_err() {
                        println!("\n  Соединение с сервером закрыто.");
                        break;
                    }
                }
            }
            Ok(None) | Err(_) => {
                if stop_signal.load(Ordering::Relaxed) {
                    break;
                }
            }
        }
    }

    if !accumulator.is_empty() {
        let bytes: Vec<u8> = accumulator
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        ws_tx.send(Message::Binary(bytes.into())).await.ok();
    }

    ws_tx.send(Message::Close(None)).await.ok();
    srv_task.abort();
    drop(stream);

    Ok(())
}

// ─── main ─────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Не удалось установить rustls crypto provider");

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
    println!("  Устройство : {}", device_name(&device));
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

    let supported_config = device
        .default_input_config()
        .context("Не удалось получить конфигурацию устройства")?;

    // cpal 0.17+: SampleRate — это просто u32, поле .0 больше не нужно
    let sample_rate_hz = supported_config.sample_rate();
    let channels_count = supported_config.channels();
    let config: StreamConfig = supported_config.into();

    println!("  ✓ Конфигурация: {} Гц, {} кан.", sample_rate_hz, channels_count);

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
            recording = true;
            let stop = Arc::new(AtomicBool::new(false));
            stop_signal = Some(stop.clone());

            let my_device_name = device_name(&device);
            let host = cpal::default_host();
            let dev = host
                .input_devices()?
                // cpal 0.18: сравниваем через description().name()
                .find(|d| device_name(d) == my_device_name)
                .or_else(|| host.default_input_device())
                .context("Устройство не найдено")?;

            let cfg    = config.clone();
            let sr     = sample_rate_hz;
            let ch     = channels_count;
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