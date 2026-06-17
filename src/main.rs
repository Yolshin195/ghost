use axum::{
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    extract::Query,
    response::{Html, IntoResponse},
    routing::get,
    Json, Router,
};
use axum_server::tls_rustls::RustlsConfig;
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    fs::{self, File},
    io::AsyncWriteExt,
    sync::Mutex,
    time::timeout,
};
use tracing::{error, info};
use ghost::{app_state::build_shared_state, handler::{
    list_sessions, get_session, delete_session,
    get_segment, update_utterance_translation, assign_speaker_to_utterance,
    health,
}};
use axum::routing::{delete, patch};

const CHUNK_DURATION_SECS: u64 = 120;
const OUTPUT_DIR: &str = "recordings";

const DB_URL: &str = "sqlite://transcripts.db";
const TEMPLATES_DIR: &str = "templates";

// ─── Состояние воркера ────────────────────────────────────────────────────────

// Магические байты начала EBML-документа (WebM/MKV)
const EBML_MAGIC: [u8; 4] = [0x1A, 0x45, 0xDF, 0xA3];

struct RecorderState {
    buffer: Vec<u8>,
    started_at: std::time::Instant,
    file_index: u32,
    session_id: String,
    /// EBML-заголовок первого WebM чанка.
    /// MediaRecorder шлёт его только один раз — в начале стрима.
    /// Без него все последующие сегменты невалидны.
    /// Сохраняем и prepend-им к каждому новому сегменту.
    webm_header: Vec<u8>,
}

impl RecorderState {
    fn new(session_id: String) -> Self {
        Self {
            buffer: Vec::new(),
            started_at: std::time::Instant::now(),
            file_index: 0,
            session_id,
            webm_header: Vec::new(),
        }
    }

    fn starts_with_ebml(data: &[u8]) -> bool {
        data.len() >= 4 && data[..4] == EBML_MAGIC
    }
}

// ─── Хендлеры ────────────────────────────────────────────────────────────────

async fn ws_handler(ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(handle_audio_socket)
}

async fn handle_audio_socket(socket: WebSocket) {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let session_id = format!("rec_{ts}");
    info!("Новая сессия: {session_id}");

    if let Err(e) = fs::create_dir_all(OUTPUT_DIR).await {
        error!("Не удалось создать {OUTPUT_DIR}: {e}");
        return;
    }

    let state = Arc::new(Mutex::new(RecorderState::new(session_id.clone())));
    let (mut sender, mut receiver) = socket.split();

    loop {
        let msg = match timeout(Duration::from_secs(30), receiver.next()).await {
            Ok(Some(Ok(m))) => m,
            Ok(Some(Err(e))) => {
                error!("WS: {e}");
                break;
            }
            Ok(None) => {
                info!("Клиент отключился");
                break;
            }
            Err(_) => {
                error!("Таймаут");
                break;
            }
        };

        match msg {
            Message::Binary(data) => {
                let mut st = state.lock().await;

                // Если чанк начинается с EBML-заголовка — это начало нового стрима.
                // Сохраняем заголовок чтобы prepend-ить его в каждый следующий сегмент.
                if RecorderState::starts_with_ebml(&data) && st.webm_header.is_empty() {
                    st.webm_header = data.to_vec();
                    info!("EBML заголовок сохранён ({} байт)", data.len());
                }

                st.buffer.extend_from_slice(&data);

                if st.started_at.elapsed() >= Duration::from_secs(CHUNK_DURATION_SECS) {
                    if let Some((fname, kb)) = flush_segment(&mut st).await {
                        let json = format!(
                            r#"{{"type":"file_saved","filename":"{fname}","size_kb":{kb}}}"#
                        );
                        if sender.send(Message::Text(json.into())).await.is_err() {
                            break;
                        }
                    }
                }
            }
            Message::Close(_) => {
                info!("Close frame");
                break;
            }
            Message::Ping(p) => {
                let _ = sender.send(Message::Pong(p)).await;
            }
            _ => {}
        }
    }

    // Финальный flush — сохраняем текущий незавершённый сегмент.
    // Вызывается при любом выходе: кнопка Стоп, закрытие вкладки, обрыв связи.
    let mut st = state.lock().await;
    if !st.buffer.is_empty() {
        info!("Финальный flush буфера ({} байт)...", st.buffer.len());
        flush_segment(&mut st).await;
    }
    info!("Сессия {session_id} завершена");
}

async fn flush_segment(st: &mut RecorderState) -> Option<(String, u64)> {
    if st.buffer.is_empty() {
        return None;
    }

    st.file_index += 1;
    let filename = format!(
        "{}/{}_seg{:04}.webm",
        OUTPUT_DIR, st.session_id, st.file_index
    );
    let data = std::mem::take(&mut st.buffer);

    // Если сегмент не начинается с EBML-заголовка — prepend сохранённый заголовок.
    // Это критично: MediaRecorder шлёт заголовок только в первом чанке,
    // без него ffmpeg/whisper не могут прочитать файл.
    let write_data: Vec<u8> =
        if !RecorderState::starts_with_ebml(&data) && !st.webm_header.is_empty() {
            let mut full = st.webm_header.clone();
            full.extend_from_slice(&data);
            full
        } else {
            data
        };

    let size_kb = write_data.len() as u64 / 1024;

    match File::create(&filename).await {
        Ok(mut f) => {
            if let Err(e) = f.write_all(&write_data).await {
                error!("Ошибка записи {filename}: {e}");
                return None;
            }
            info!("Сохранён: {filename} ({size_kb} KB)");
        }
        Err(e) => {
            error!("Создание {filename}: {e}");
            return None;
        }
    }

    st.started_at = std::time::Instant::now();
    let short = PathBuf::from(&filename)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(&filename)
        .to_string();
    Some((short, size_kb))
}

// ─── Transcripts API ─────────────────────────────────────────────────────────

#[derive(Serialize)]
struct Segment {
    filename: String,
    index: u32,
    text: Option<String>, // None = ещё не транскрибировано
    has_audio: bool,
}

#[derive(Serialize)]
struct Session {
    id: String, // rec_XXXXXXXXXX
    segments: Vec<Segment>,
    total_text: String, // весь текст сессии одной строкой
}

async fn api_transcripts() -> impl IntoResponse {
    let mut sessions: std::collections::BTreeMap<String, Vec<Segment>> =
        std::collections::BTreeMap::new();

    let mut rd = match tokio::fs::read_dir(OUTPUT_DIR).await {
        Ok(r) => r,
        Err(_) => return Json(Vec::<Session>::new()),
    };

    while let Ok(Some(entry)) = rd.next_entry().await {
        let name = entry.file_name().to_string_lossy().to_string();

        let is_webm = name.ends_with(".webm");
        let is_wav = name.ends_with(".wav");
        if !is_webm && !is_wav {
            continue;
        }

        let ext = if is_wav { ".wav" } else { ".webm" };

        // rec_1781188527_seg0001.webm / cli_1781188527_seg0001.wav
        let Some(session_id) = name.rsplitn(2, "_seg").last().map(|s| s.to_string()) else {
            continue;
        };

        // индекс сегмента
        let index: u32 = name
            .trim_end_matches(ext)
            .rsplitn(2, "_seg")
            .next()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        let txt_path = format!("{}/{}", OUTPUT_DIR, name.replace(ext, ".txt"));
        let text = tokio::fs::read_to_string(&txt_path)
            .await
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        sessions.entry(session_id).or_default().push(Segment {
            filename: name,
            index,
            text,
            has_audio: true,
        });
    }

    let result: Vec<Session> = sessions
        .into_iter()
        .rev()
        .map(|(id, mut segs)| {
            segs.sort_by_key(|s| s.index);
            let total_text = segs
                .iter()
                .filter_map(|s| s.text.as_deref())
                .collect::<Vec<_>>()
                .join("\n");
            Session {
                id,
                segments: segs,
                total_text,
            }
        })
        .collect();

    Json(result)
}



// ═══════════════════════════════════════════════════════════════════════════
// ПАТЧ К СЕРВЕРУ (main.rs)
//
// Добавить этот код для поддержки консольного клиента.
// Консольный клиент шлёт raw PCM f32-le по ws/audio-pcm?sr=16000&ch=1
// Сервер оборачивает в валидный .wav и сохраняет рядом с .webm сегментами.
// ═══════════════════════════════════════════════════════════════════════════

// ── 1. Добавить в Cargo.toml (зависимости сервера) ───────────────────────
//
// [dependencies]
// axum          = { version = "0.7", features = ["ws"] }
// # ... остальные как были ...
//
// (ничего нового не нужно — WAV пишем руками)

// ── 2. Новые типы / функции — вставить перед fn main() ───────────────────

/// Строит WAV-заголовок для PCM f32le.
/// data_bytes — размер секции data (может быть 0 при стриминге — обновим потом).
fn make_wav_header(sample_rate: u32, channels: u16, data_bytes: u32) -> Vec<u8> {
    let byte_rate = sample_rate * channels as u32 * 2; // 16-bit = 2 bytes
    let chunk_size = 36 + data_bytes;

    let mut h = Vec::with_capacity(44);
    h.extend_from_slice(b"RIFF");
    h.extend_from_slice(&chunk_size.to_le_bytes());
    h.extend_from_slice(b"WAVE");
    h.extend_from_slice(b"fmt ");
    h.extend_from_slice(&16u32.to_le_bytes());
    h.extend_from_slice(&1u16.to_le_bytes()); // PCM int = 1
    h.extend_from_slice(&channels.to_le_bytes());
    h.extend_from_slice(&sample_rate.to_le_bytes());
    h.extend_from_slice(&byte_rate.to_le_bytes());
    h.extend_from_slice(&(channels * 2).to_le_bytes()); // block align
    h.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    h.extend_from_slice(b"data");
    h.extend_from_slice(&data_bytes.to_le_bytes());
    h
}

/// Хендлер апгрейда WebSocket для PCM-клиента.
/// Query-параметры: ?sr=16000&ch=1  (частота и число каналов)
async fn ws_pcm_handler(
    ws: WebSocketUpgrade,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let sample_rate: u32 = params
        .get("sr")
        .and_then(|v| v.parse().ok())
        .unwrap_or(16000);
    let channels: u16 = params.get("ch").and_then(|v| v.parse().ok()).unwrap_or(1);

    ws.on_upgrade(move |socket| handle_pcm_socket(socket, sample_rate, channels))
}

async fn handle_pcm_socket(socket: WebSocket, sample_rate: u32, channels: u16) {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let session_id = format!("cli_{ts}");
    info!("PCM сессия: {session_id}  ({sample_rate} Гц, {channels} кан.)");

    if let Err(e) = fs::create_dir_all(OUTPUT_DIR).await {
        error!("Не удалось создать {OUTPUT_DIR}: {e}");
        return;
    }

    let (mut sender, mut receiver) = socket.split();

    // Буфер сегмента (только PCM-данные, без WAV-заголовка)
    let mut pcm_buf: Vec<u8> = Vec::new();
    let mut started_at = std::time::Instant::now();
    let mut file_index: u32 = 0;

    /// Сбрасывает буфер в .wav файл, возвращает (filename, size_kb)
    async fn flush_wav(
        session_id: &str,
        file_index: &mut u32,
        pcm_buf: &mut Vec<u8>,
        started_at: &mut std::time::Instant,
        sample_rate: u32,
        channels: u16,
    ) -> Option<(String, u64)> {
        if pcm_buf.is_empty() {
            return None;
        }

        *file_index += 1;
        let filename = format!("{}/{}_seg{:04}.wav", OUTPUT_DIR, session_id, file_index);

        let raw_bytes = std::mem::take(pcm_buf);

        // ── 1. Декодируем f32-le байты в сэмплы ──────────────────────────────
        let mut samples: Vec<f32> = raw_bytes
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();

        // ── 2. Ресемплинг в 16 000 Гц (если нужно) ───────────────────────────
        // Простой linear interpolation — достаточно для речи.
        let out_rate: u32 = 16_000;
        let samples_16k: Vec<f32> = if sample_rate != out_rate {
            let ratio = sample_rate as f64 / out_rate as f64;
            let out_len = (samples.len() as f64 / ratio).ceil() as usize;
            let mut out = Vec::with_capacity(out_len);
            for i in 0..out_len {
                let pos = i as f64 * ratio;
                let lo = pos.floor() as usize;
                let hi = (lo + 1).min(samples.len() - 1);
                let t = (pos - lo as f64) as f32;
                out.push(samples[lo] * (1.0 - t) + samples[hi] * t);
            }
            out
        } else {
            samples.clone()
        };
        samples = samples_16k;

        // ── 3. Нормализация по пику ───────────────────────────────────────────
        // Целевой пик -1 dBFS ≈ 0.891. Если сигнал уже громче — не трогаем.
        const TARGET_PEAK: f32 = 0.891;
        let peak = samples.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
        if peak > 1e-6 {
            let gain = (TARGET_PEAK / peak).min(20.0); // ограничение ×20 (~26 dB)
            for s in &mut samples {
                *s *= gain;
            }
            let gain_db = 20.0 * gain.log10();
            info!(
                "Нормализация: peak={:.1} dBFS → gain +{:.1} dB",
                20.0 * peak.log10(),
                gain_db
            );
        }

        // ── 4. Конвертация f32 → i16 ─────────────────────────────────────────
        let pcm_i16: Vec<i16> = samples
            .iter()
            .map(|&s| (s.clamp(-1.0, 1.0) * 32767.0) as i16)
            .collect();

        let data_bytes = (pcm_i16.len() * 2) as u32; // 2 bytes per i16
        let mut wav = make_wav_header(out_rate, channels, data_bytes);
        for sample in &pcm_i16 {
            wav.extend_from_slice(&sample.to_le_bytes());
        }

        let size_kb = wav.len() as u64 / 1024;

        match File::create(&filename).await {
            Ok(mut f) => {
                if let Err(e) = f.write_all(&wav).await {
                    error!("Ошибка записи {filename}: {e}");
                    return None;
                }
                info!("WAV сохранён: {filename} ({size_kb} KB)  [{sample_rate}→{out_rate}Hz, нормализован]");
            }
            Err(e) => {
                error!("Создание {filename}: {e}");
                return None;
            }
        }

        *started_at = std::time::Instant::now();
        let short = PathBuf::from(&filename)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&filename)
            .to_string();
        Some((short, size_kb))
    }

    loop {
        let msg = match timeout(Duration::from_secs(30), receiver.next()).await {
            Ok(Some(Ok(m))) => m,
            Ok(Some(Err(e))) => {
                error!("PCM WS: {e}");
                break;
            }
            Ok(None) => {
                info!("PCM клиент отключился");
                break;
            }
            Err(_) => {
                error!("PCM таймаут");
                break;
            }
        };

        match msg {
            Message::Binary(data) => {
                pcm_buf.extend_from_slice(&data);

                // Сегментируем по времени (те же 2 минуты)
                if started_at.elapsed() >= Duration::from_secs(CHUNK_DURATION_SECS) {
                    if let Some((fname, kb)) = flush_wav(
                        &session_id,
                        &mut file_index,
                        &mut pcm_buf,
                        &mut started_at,
                        sample_rate,
                        channels,
                    )
                    .await
                    {
                        let json = format!(
                            r#"{{"type":"file_saved","filename":"{fname}","size_kb":{kb}}}"#
                        );
                        if sender.send(Message::Text(json.into())).await.is_err() {
                            break;
                        }
                    }
                }
            }
            Message::Close(_) => {
                info!("PCM Close frame");
                break;
            }
            Message::Ping(p) => {
                let _ = sender.send(Message::Pong(p)).await;
            }
            _ => {}
        }
    }

    // Финальный flush
    if !pcm_buf.is_empty() {
        info!("Финальный WAV flush ({} байт)...", pcm_buf.len());
        flush_wav(
            &session_id,
            &mut file_index,
            &mut pcm_buf,
            &mut started_at,
            sample_rate,
            channels,
        )
        .await;
    }
    info!("PCM сессия {session_id} завершена");
}

// ─── раздача статики ─────────────────────────────────────────────────────────

async fn sessions_page() -> impl IntoResponse {
    let path = format!("{TEMPLATES_DIR}/sessions.html");
    match tokio::fs::read_to_string(&path).await {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            error!("Не удалось прочитать шаблон {path}: {e}");
            axum::http::StatusCode::NOT_FOUND.into_response()
        }
    }
}

async fn recorder_page() -> impl IntoResponse {
    let path = format!("{TEMPLATES_DIR}/recorder.html");
    match tokio::fs::read_to_string(&path).await {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            error!("Шаблон {path}: {e}");
            axum::http::StatusCode::NOT_FOUND.into_response()
        }
    }
}

async fn transcripts_page() -> impl IntoResponse {
    let path = format!("{TEMPLATES_DIR}/transcripts.html");
    match tokio::fs::read_to_string(&path).await {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            error!("Шаблон {path}: {e}");
            axum::http::StatusCode::NOT_FOUND.into_response()
        }
    }
}

// ─── main ────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("Не удалось установить rustls crypto provider");

    tracing_subscriber::fmt()
        .with_target(false)
        .with_max_level(tracing::Level::INFO)
        .init();

    // Инициализация БД (логика в src/app_state.rs)
    let shared_state = build_shared_state(DB_URL)
        .await
        .expect("Не удалось подключиться к БД");

    let app = Router::new()
        // Страницы
        .route("/", get(recorder_page))
        .route("/sessions", get(sessions_page))
        .route("/transcripts", get(transcripts_page))      // старая страница
        // WebSocket
        .route("/ws/audio", get(ws_handler))
        .route("/ws/audio-pcm", get(ws_pcm_handler))
        // API транскриптов (старый)
        .route("/api/transcripts", get(api_transcripts))
        // API сессий (новый, через БД)
        .route("/api/sessions", get(list_sessions))
        .route("/api/sessions/{id}", get(get_session))
        .route("/api/sessions/{id}", delete(delete_session))
        .route("/api/segments/{id}", get(get_segment))
        .route("/api/utterances/{id}/translation", patch(update_utterance_translation))
        .route("/api/utterances/{id}/speaker", axum::routing::post(assign_speaker_to_utterance))
        .route("/health", get(health))
        // Подключаем SharedState
        .with_state(shared_state);

    // Сертификат и ключ генерируются build.rs автоматически при сборке
    let tls = RustlsConfig::from_pem_file("cert.pem", "key.pem")
        .await
        .expect("Не удалось загрузить TLS-сертификат. Запусти `cargo build` для генерации.");

    let addr: std::net::SocketAddr = "0.0.0.0:3005".parse().unwrap();
    info!("Сервер: https://{addr}  →  записи в ./{OUTPUT_DIR}/");
    info!("На телефоне: откройте https://<IP>:3005 и примите предупреждение о сертификате");

    axum_server::bind_rustls(addr, tls)
        .serve(app.into_make_service())
        .await
        .unwrap();
}
