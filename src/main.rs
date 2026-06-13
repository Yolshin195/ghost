use axum::{
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    response::{Html, IntoResponse},
    routing::get,
    Router,
};
use futures_util::{SinkExt, StreamExt};
use std::{
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

const CHUNK_DURATION_SECS: u64 = 120; // 2 минуты
const OUTPUT_DIR: &str = "recordings";

// ─── HTML страница ────────────────────────────────────────────────────────────

const HTML: &str = r#"<!DOCTYPE html>
<html lang="ru">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>Аудио запись</title>
<style>
  :root {
    --bg: #0d0f14;
    --surface: #151820;
    --border: #1e2330;
    --accent: #4f8ef7;
    --accent-dim: #1e3260;
    --danger: #e05555;
    --danger-dim: #3a1515;
    --text: #c8d0e0;
    --text-dim: #5a6480;
    --green: #3ecf8e;
    --green-dim: #0e3025;
    --mono: 'JetBrains Mono', 'Fira Code', 'Courier New', monospace;
  }

  * { box-sizing: border-box; margin: 0; padding: 0; }

  body {
    background: var(--bg);
    color: var(--text);
    font-family: var(--mono);
    min-height: 100vh;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 32px;
    padding: 24px;
  }

  .header {
    text-align: center;
  }

  .title {
    font-size: 13px;
    letter-spacing: 0.2em;
    text-transform: uppercase;
    color: var(--text-dim);
    margin-bottom: 8px;
  }

  .subtitle {
    font-size: 22px;
    font-weight: 600;
    color: var(--text);
    letter-spacing: -0.02em;
  }

  .card {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 12px;
    padding: 28px 32px;
    width: 100%;
    max-width: 440px;
    display: flex;
    flex-direction: column;
    gap: 20px;
  }

  /* Визуализатор */
  .visualizer-wrap {
    position: relative;
    height: 64px;
    border-radius: 8px;
    overflow: hidden;
    background: var(--bg);
    border: 1px solid var(--border);
  }

  canvas#viz {
    width: 100%;
    height: 100%;
    display: block;
  }

  .viz-idle {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: 11px;
    color: var(--text-dim);
    letter-spacing: 0.15em;
    text-transform: uppercase;
    pointer-events: none;
  }

  /* Статусы */
  .status-row {
    display: flex;
    align-items: center;
    gap: 10px;
    font-size: 12px;
    color: var(--text-dim);
  }

  .dot {
    width: 8px; height: 8px;
    border-radius: 50%;
    background: var(--text-dim);
    flex-shrink: 0;
    transition: background 0.3s;
  }
  .dot.connecting { background: #f7c44f; }
  .dot.recording  { background: var(--danger); animation: pulse 1.2s ease-in-out infinite; }
  .dot.ok         { background: var(--green); }

  @keyframes pulse {
    0%, 100% { opacity: 1; }
    50%       { opacity: 0.3; }
  }

  #status-text { transition: color 0.3s; }

  /* Прогресс сегмента */
  .segment-row {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .segment-label {
    display: flex;
    justify-content: space-between;
    font-size: 11px;
    color: var(--text-dim);
    letter-spacing: 0.05em;
  }

  .progress-track {
    height: 4px;
    border-radius: 2px;
    background: var(--border);
    overflow: hidden;
  }

  .progress-fill {
    height: 100%;
    width: 0%;
    border-radius: 2px;
    background: var(--accent);
    transition: width 0.5s linear, background 0.3s;
  }

  /* Счётчики */
  .counters {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 12px;
  }

  .counter-box {
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 12px 14px;
  }

  .counter-label {
    font-size: 10px;
    color: var(--text-dim);
    letter-spacing: 0.12em;
    text-transform: uppercase;
    margin-bottom: 4px;
  }

  .counter-value {
    font-size: 20px;
    font-weight: 700;
    color: var(--text);
    letter-spacing: -0.03em;
  }

  /* Кнопка */
  .btn {
    padding: 12px 24px;
    border-radius: 8px;
    border: none;
    font-family: var(--mono);
    font-size: 13px;
    font-weight: 600;
    letter-spacing: 0.05em;
    cursor: pointer;
    transition: background 0.2s, transform 0.1s;
    width: 100%;
  }

  .btn:active { transform: scale(0.98); }
  .btn:disabled { opacity: 0.4; cursor: not-allowed; transform: none; }

  .btn-start {
    background: var(--accent);
    color: #fff;
  }
  .btn-start:hover:not(:disabled) { background: #6fa3ff; }

  .btn-stop {
    background: var(--danger-dim);
    color: var(--danger);
    border: 1px solid var(--danger);
  }
  .btn-stop:hover:not(:disabled) { background: #4a1a1a; }

  /* Лог файлов */
  .log-header {
    font-size: 11px;
    color: var(--text-dim);
    letter-spacing: 0.12em;
    text-transform: uppercase;
    margin-bottom: 8px;
  }

  #file-log {
    max-height: 140px;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .log-entry {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 11px;
    color: var(--text-dim);
    padding: 6px 10px;
    background: var(--bg);
    border-radius: 5px;
    border: 1px solid var(--border);
    animation: fadeIn 0.3s ease;
  }

  .log-entry .icon { color: var(--green); }
  .log-entry .fname { color: var(--text); flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .log-entry .size { color: var(--text-dim); flex-shrink: 0; }

  @keyframes fadeIn {
    from { opacity: 0; transform: translateY(-4px); }
    to   { opacity: 1; transform: translateY(0); }
  }

  #file-log:empty::after {
    content: 'Файлы появятся здесь';
    font-size: 11px;
    color: var(--text-dim);
    padding: 8px 10px;
    text-align: center;
    display: block;
  }
</style>
</head>
<body>

<div class="header">
  <div class="title">Stream Recorder</div>
  <div class="subtitle">Аудио → Сервер → Файлы</div>
</div>

<div class="card">
  <!-- Визуализатор -->
  <div class="visualizer-wrap">
    <canvas id="viz"></canvas>
    <div class="viz-idle" id="viz-idle">Ожидание микрофона</div>
  </div>

  <!-- Статус -->
  <div class="status-row">
    <div class="dot" id="dot"></div>
    <span id="status-text">Готов к записи</span>
  </div>

  <!-- Прогресс сегмента -->
  <div class="segment-row">
    <div class="segment-label">
      <span>Текущий сегмент</span>
      <span id="seg-time">0:00 / 2:00</span>
    </div>
    <div class="progress-track">
      <div class="progress-fill" id="seg-fill"></div>
    </div>
  </div>

  <!-- Счётчики -->
  <div class="counters">
    <div class="counter-box">
      <div class="counter-label">Время записи</div>
      <div class="counter-value" id="total-time">0:00</div>
    </div>
    <div class="counter-box">
      <div class="counter-label">Файлов сохранено</div>
      <div class="counter-value" id="file-count">0</div>
    </div>
  </div>

  <!-- Кнопка -->
  <button class="btn btn-start" id="main-btn" onclick="toggleRecording()">
    ▶ Начать запись
  </button>
</div>

<!-- Лог файлов -->
<div class="card">
  <div class="log-header">Сохранённые файлы</div>
  <div id="file-log"></div>
</div>

<script>
let ws = null;
let mediaRecorder = null;
let audioCtx = null;
let analyser = null;
let stream = null;
let animId = null;
let segTimer = null;
let totalTimer = null;
let isRecording = false;

let totalSecs = 0;
let segSecs = 0;
let fileCount = 0;
const SEG_DURATION = 120;

const dot       = document.getElementById('dot');
const statusTxt = document.getElementById('status-text');
const segFill   = document.getElementById('seg-fill');
const segTime   = document.getElementById('seg-time');
const totalTimeEl = document.getElementById('total-time');
const fileCountEl = document.getElementById('file-count');
const mainBtn   = document.getElementById('main-btn');
const vizIdle   = document.getElementById('viz-idle');
const canvas    = document.getElementById('viz');
const ctx2d     = canvas.getContext('2d');

function setStatus(state, text) {
  dot.className = 'dot ' + state;
  statusTxt.textContent = text;
}

function fmt(s) {
  return `${Math.floor(s/60)}:${String(s%60).padStart(2,'0')}`;
}

function updateSegProgress() {
  const pct = (segSecs / SEG_DURATION) * 100;
  segFill.style.width = pct + '%';
  segFill.style.background = pct > 80 ? 'var(--green)' : 'var(--accent)';
  segTime.textContent = `${fmt(segSecs)} / 2:00`;
}

async function toggleRecording() {
  if (isRecording) {
    stopRecording();
  } else {
    await startRecording();
  }
}

async function startRecording() {
  try {
    setStatus('connecting', 'Запрос микрофона...');
    mainBtn.disabled = true;

    stream = await navigator.mediaDevices.getUserMedia({ audio: true, video: false });

    // Визуализатор
    audioCtx = new AudioContext();
    analyser = audioCtx.createAnalyser();
    analyser.fftSize = 256;
    audioCtx.createMediaStreamSource(stream).connect(analyser);
    vizIdle.style.display = 'none';
    drawViz();

    // WebSocket
    const proto = location.protocol === 'https:' ? 'wss' : 'ws';
    ws = new WebSocket(`${proto}://${location.host}/ws/audio`);
    ws.binaryType = 'arraybuffer';

    ws.onopen = () => {
      setStatus('recording', 'Запись идёт...');
      startMediaRecorder();
    };

    ws.onmessage = (e) => {
      try {
        const msg = JSON.parse(e.data);
        if (msg.type === 'file_saved') {
          fileCount++;
          fileCountEl.textContent = fileCount;
          addFileLog(msg.filename, msg.size_kb);
          // сброс прогресса сегмента
          segSecs = 0;
          updateSegProgress();
        }
      } catch (_) {}
    };

    ws.onclose = () => {
      if (isRecording) {
        setStatus('connecting', 'Соединение прервано');
      }
    };

    ws.onerror = () => setStatus('', 'Ошибка WebSocket');

    // Таймеры
    totalTimer = setInterval(() => {
      totalSecs++;
      totalTimeEl.textContent = fmt(totalSecs);
    }, 1000);

    segTimer = setInterval(() => {
      segSecs++;
      updateSegProgress();
    }, 1000);

    isRecording = true;
    mainBtn.disabled = false;
    mainBtn.className = 'btn btn-stop';
    mainBtn.textContent = '■ Остановить запись';

  } catch (err) {
    console.error(err);
    setStatus('', 'Ошибка: ' + err.message);
    mainBtn.disabled = false;
  }
}

function startMediaRecorder() {
  // Пробуем webm/opus, fallback на что поддерживается
  const mimeType = MediaRecorder.isTypeSupported('audio/webm;codecs=opus')
    ? 'audio/webm;codecs=opus'
    : MediaRecorder.isTypeSupported('audio/webm')
      ? 'audio/webm'
      : '';

  const opts = mimeType ? { mimeType } : {};
  mediaRecorder = new MediaRecorder(stream, opts);

  mediaRecorder.ondataavailable = (e) => {
    if (e.data && e.data.size > 0 && ws && ws.readyState === WebSocket.OPEN) {
      e.data.arrayBuffer().then(buf => ws.send(buf));
    }
  };

  // Чанки каждые 250мс — мелко, чтобы стрим был плавным
  mediaRecorder.start(250);
}

function stopRecording() {
  isRecording = false;

  if (mediaRecorder && mediaRecorder.state !== 'inactive') mediaRecorder.stop();
  if (stream) stream.getTracks().forEach(t => t.stop());
  if (ws) ws.close();
  if (audioCtx) audioCtx.close();
  if (animId) cancelAnimationFrame(animId);
  clearInterval(totalTimer);
  clearInterval(segTimer);

  ws = null; mediaRecorder = null; stream = null; audioCtx = null; analyser = null;

  vizIdle.style.display = 'flex';
  setStatus('', 'Запись остановлена');
  mainBtn.className = 'btn btn-start';
  mainBtn.textContent = '▶ Начать запись';
}

function addFileLog(fname, sizeKb) {
  const el = document.createElement('div');
  el.className = 'log-entry';
  el.innerHTML = `
    <span class="icon">✓</span>
    <span class="fname">${fname}</span>
    <span class="size">${sizeKb} KB</span>
  `;
  const log = document.getElementById('file-log');
  log.prepend(el);
}

// ── Визуализатор ──────────────────────────────────────────────────────────────
function drawViz() {
  if (!analyser) return;
  animId = requestAnimationFrame(drawViz);

  const W = canvas.offsetWidth;
  const H = canvas.offsetHeight;
  canvas.width  = W;
  canvas.height = H;

  const bufLen = analyser.frequencyBinCount;
  const data   = new Uint8Array(bufLen);
  analyser.getByteFrequencyData(data);

  ctx2d.clearRect(0, 0, W, H);

  const barW = W / bufLen * 2.5;
  let x = 0;

  for (let i = 0; i < bufLen; i++) {
    const v   = data[i] / 255;
    const h   = v * H;
    const hue = 200 + v * 40; // синий→голубой

    ctx2d.fillStyle = `hsla(${hue}, 80%, ${40 + v*30}%, ${0.4 + v*0.6})`;
    ctx2d.fillRect(x, H - h, barW - 1, h);
    x += barW + 1;
    if (x > W) break;
  }
}
</script>
</body>
</html>
"#;

// ─── Состояние записывающего воркера ─────────────────────────────────────────

struct RecorderState {
    buffer: Vec<u8>,
    started_at: std::time::Instant,
    file_index: u32,
    session_id: String,
}

impl RecorderState {
    fn new(session_id: String) -> Self {
        Self {
            buffer: Vec::new(),
            started_at: std::time::Instant::now(),
            file_index: 0,
            session_id,
        }
    }
}

// ─── Хендлеры ────────────────────────────────────────────────────────────────

async fn index() -> impl IntoResponse {
    Html(HTML)
}

async fn ws_handler(ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(handle_audio_socket)
}

async fn handle_audio_socket(socket: WebSocket) {
    let session_id = {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        format!("rec_{ts}")
    };

    info!("Новая сессия записи: {session_id}");

    // Создаём папку для записей
    if let Err(e) = fs::create_dir_all(OUTPUT_DIR).await {
        error!("Не удалось создать директорию {OUTPUT_DIR}: {e}");
        return;
    }

    let state = Arc::new(Mutex::new(RecorderState::new(session_id.clone())));
    let (mut sender, mut receiver) = socket.split();

    loop {
        // Ждём очередное сообщение (с небольшим таймаутом чтобы не висеть вечно)
        let msg = match timeout(Duration::from_secs(30), receiver.next()).await {
            Ok(Some(Ok(msg))) => msg,
            Ok(Some(Err(e))) => {
                error!("WS ошибка: {e}");
                break;
            }
            Ok(None) => {
                info!("Клиент закрыл соединение");
                break;
            }
            Err(_) => {
                error!("Таймаут ожидания данных");
                break;
            }
        };

        match msg {
            Message::Binary(data) => {
                let mut st = state.lock().await;
                st.buffer.extend_from_slice(&data);

                // Проверяем — не пора ли нарезать файл
                if st.started_at.elapsed() >= Duration::from_secs(CHUNK_DURATION_SECS) {
                    if let Some(info) = flush_segment(&mut st).await {
                        // Сообщаем браузеру
                        let json = format!(
                            r#"{{"type":"file_saved","filename":"{}","size_kb":{}}}"#,
                            info.0, info.1
                        );
                        if sender.send(Message::Text(json)).await.is_err() {
                            break;
                        }
                    }
                }
            }
            Message::Close(_) => {
                info!("Close frame получен");
                break;
            }
            Message::Ping(p) => {
                let _ = sender.send(Message::Pong(p)).await;
            }
            _ => {}
        }
    }

    // Сохраняем остаток при закрытии соединения
    let mut st = state.lock().await;
    if !st.buffer.is_empty() {
        flush_segment(&mut st).await;
    }

    info!("Сессия {session_id} завершена");
}

/// Сбрасывает текущий буфер на диск. Возвращает (имя файла, размер в KB).
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
    let size_kb = data.len() as u64 / 1024;
    let path = PathBuf::from(&filename);

    match File::create(&path).await {
        Ok(mut f) => {
            if let Err(e) = f.write_all(&data).await {
                error!("Ошибка записи {filename}: {e}");
                return None;
            }
            info!("Сохранён файл: {filename} ({size_kb} KB)");
        }
        Err(e) => {
            error!("Не удалось создать файл {filename}: {e}");
            return None;
        }
    }

    // Сбрасываем таймер сегмента
    st.started_at = std::time::Instant::now();

    let short_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(&filename)
        .to_string();

    Some((short_name, size_kb))
}

// ─── main ────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_target(false)
        .with_max_level(tracing::Level::INFO)
        .init();

    let app = Router::new()
        .route("/", get(index))
        .route("/ws/audio", get(ws_handler));

    let addr = "0.0.0.0:3000";
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();

    info!("Сервер запущен: http://{addr}");
    info!("Файлы будут сохраняться в ./{OUTPUT_DIR}/");

    axum::serve(listener, app).await.unwrap();
}