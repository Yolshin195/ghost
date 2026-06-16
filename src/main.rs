use axum::{
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    response::{Html, IntoResponse},
    routing::get,
    Json, Router,
};
use serde::Serialize;
use axum_server::tls_rustls::RustlsConfig;
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

const CHUNK_DURATION_SECS: u64 = 120;
const OUTPUT_DIR: &str = "recordings";

const HTML: &str = r#"<!DOCTYPE html>
<html lang="ru">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>Аудио запись</title>
<style>
  :root {
    --bg:         #0d0f14;
    --surface:    #151820;
    --surface2:   #1a1e28;
    --border:     #1e2330;
    --accent:     #4f8ef7;
    --danger:     #e05555;
    --danger-dim: #3a1515;
    --text:       #c8d0e0;
    --text-dim:   #5a6480;
    --green:      #3ecf8e;
    --yellow:     #f7c44f;
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
    padding: 32px 16px 48px;
    gap: 16px;
  }

  .page-title { font-size: 11px; letter-spacing: .2em; text-transform: uppercase; color: var(--text-dim); margin-bottom: 4px; }
  .page-sub   { font-size: 20px; font-weight: 700; letter-spacing: -.02em; }

  .card {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 12px;
    padding: 20px 22px;
    width: 100%; max-width: 480px;
    display: flex; flex-direction: column; gap: 16px;
  }
  .card-title {
    font-size: 10px; letter-spacing: .15em; text-transform: uppercase;
    color: var(--text-dim); padding-bottom: 10px;
    border-bottom: 1px solid var(--border);
  }

  .viz-wrap {
    position: relative; height: 72px; border-radius: 8px;
    overflow: hidden; background: var(--bg); border: 1px solid var(--border);
  }
  canvas#viz { width: 100%; height: 100%; display: block; }
  .viz-idle {
    position: absolute; inset: 0; display: flex; align-items: center;
    justify-content: center; font-size: 11px; color: var(--text-dim);
    letter-spacing: .15em; text-transform: uppercase; pointer-events: none;
  }

  .vu-row { display: flex; align-items: center; gap: 10px; }
  .vu-label { font-size: 10px; color: var(--text-dim); width: 36px; flex-shrink: 0; }
  .vu-track { flex: 1; height: 6px; background: var(--border); border-radius: 3px; overflow: hidden; }
  .vu-fill  { height: 100%; width: 0%; border-radius: 3px; transition: width .08s; background: var(--green); }
  .vu-val   { font-size: 11px; color: var(--text-dim); width: 44px; text-align: right; flex-shrink: 0; }

  .settings-grid { display: flex; flex-direction: column; gap: 14px; }
  .setting-row   { display: flex; flex-direction: column; gap: 6px; }
  .setting-head  { display: flex; justify-content: space-between; align-items: center; }
  .setting-name  { font-size: 12px; color: var(--text); }
  .setting-hint  { font-size: 10px; color: var(--text-dim); }
  .setting-val   { font-size: 12px; color: var(--accent); min-width: 40px; text-align: right; }

  input[type=range] {
    -webkit-appearance: none; appearance: none;
    width: 100%; height: 4px; border-radius: 2px;
    background: var(--border); outline: none; cursor: pointer;
  }
  input[type=range]::-webkit-slider-thumb {
    -webkit-appearance: none; appearance: none;
    width: 16px; height: 16px; border-radius: 50%;
    background: var(--accent); cursor: pointer;
    border: 2px solid var(--bg); transition: background .15s;
  }
  input[type=range]::-webkit-slider-thumb:hover { background: #7ab0ff; }
  input[type=range]:disabled { opacity: .35; cursor: not-allowed; }

  .toggle-group { display: flex; gap: 6px; flex-wrap: wrap; }
  .toggle-btn {
    padding: 5px 12px; border-radius: 5px; border: 1px solid var(--border);
    background: var(--bg); color: var(--text-dim); font-family: var(--mono);
    font-size: 11px; cursor: pointer; transition: all .15s;
  }
  .toggle-btn:hover { border-color: var(--accent); color: var(--text); }
  .toggle-btn.on    { background: var(--accent-dim,#1e3260); border-color: var(--accent); color: var(--accent); }

  select {
    width: 100%; padding: 8px 10px; border-radius: 7px;
    background: var(--bg); border: 1px solid var(--border);
    color: var(--text); font-family: var(--mono); font-size: 12px;
    outline: none; cursor: pointer;
  }
  select:focus { border-color: var(--accent); }
  select:disabled { opacity: .35; cursor: not-allowed; }

  .sep { height: 1px; background: var(--border); }

  .status-row { display: flex; align-items: center; gap: 10px; font-size: 12px; color: var(--text-dim); }
  .dot {
    width: 8px; height: 8px; border-radius: 50%; background: var(--text-dim);
    flex-shrink: 0; transition: background .3s;
  }
  .dot.connecting { background: var(--yellow); }
  .dot.recording  { background: var(--danger); animation: pulse 1.2s ease-in-out infinite; }
  .dot.ok         { background: var(--green); }
  @keyframes pulse { 0%,100%{opacity:1} 50%{opacity:.3} }

  .seg-label { display: flex; justify-content: space-between; font-size: 11px; color: var(--text-dim); }
  .progress-track { height: 4px; border-radius: 2px; background: var(--border); overflow: hidden; }
  .progress-fill  { height: 100%; width: 0%; border-radius: 2px; background: var(--accent); transition: width .5s linear, background .3s; }

  .counters { display: grid; grid-template-columns: 1fr 1fr; gap: 10px; }
  .counter-box { background: var(--bg); border: 1px solid var(--border); border-radius: 8px; padding: 10px 14px; }
  .counter-label { font-size: 10px; color: var(--text-dim); letter-spacing: .12em; text-transform: uppercase; margin-bottom: 4px; }
  .counter-value { font-size: 20px; font-weight: 700; letter-spacing: -.03em; }

  .btn {
    padding: 12px 24px; border-radius: 8px; border: none;
    font-family: var(--mono); font-size: 13px; font-weight: 600;
    letter-spacing: .05em; cursor: pointer;
    transition: background .2s, transform .1s; width: 100%;
  }
  .btn:active { transform: scale(.98); }
  .btn:disabled { opacity: .4; cursor: not-allowed; transform: none; }
  .btn-start { background: var(--accent); color: #fff; }
  .btn-start:hover:not(:disabled) { background: #6fa3ff; }
  .btn-stop  { background: var(--danger-dim); color: var(--danger); border: 1px solid var(--danger); }
  .btn-stop:hover:not(:disabled) { background: #4a1a1a; }

  #file-log { max-height: 140px; overflow-y: auto; display: flex; flex-direction: column; gap: 4px; }
  .log-entry {
    display: flex; align-items: center; gap: 8px;
    font-size: 11px; color: var(--text-dim);
    padding: 6px 10px; background: var(--bg);
    border-radius: 5px; border: 1px solid var(--border);
    animation: fadeIn .3s ease;
  }
  .log-entry .icon  { color: var(--green); }
  .log-entry .fname { color: var(--text); flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .log-entry .size  { color: var(--text-dim); flex-shrink: 0; }
  @keyframes fadeIn { from{opacity:0;transform:translateY(-4px)} to{opacity:1;transform:translateY(0)} }
  #file-log:empty::after {
    content: 'Файлы появятся здесь'; font-size: 11px; color: var(--text-dim);
    padding: 8px 10px; text-align: center; display: block;
  }
</style>
</head>
<body>

<div style="text-align:center;margin-bottom:4px">
  <div class="page-title">Stream Recorder</div>
  <div class="page-sub">Аудио → Сервер → Файлы</div>
</div>

<div class="card">
  <div class="viz-wrap">
    <canvas id="viz"></canvas>
    <div class="viz-idle" id="viz-idle">Ожидание микрофона</div>
  </div>

  <div class="vu-row">
    <span class="vu-label">Уровень</span>
    <div class="vu-track"><div class="vu-fill" id="vu-fill"></div></div>
    <span class="vu-val" id="vu-val">— дБ</span>
  </div>

  <div class="status-row">
    <div class="dot" id="dot"></div>
    <span id="status-text">Готов к записи</span>
  </div>

  <div style="display:flex;flex-direction:column;gap:6px">
    <div class="seg-label">
      <span>Текущий сегмент</span>
      <span id="seg-time">0:00 / 2:00</span>
    </div>
    <div class="progress-track"><div class="progress-fill" id="seg-fill"></div></div>
  </div>

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

  <button class="btn btn-start" id="main-btn" onclick="toggleRecording()">▶ Начать запись</button>
</div>

<div class="card">
  <div class="card-title">Настройки микрофона</div>
  <div class="settings-grid">

    <div class="setting-row">
      <div class="setting-head">
        <span class="setting-name">Устройство</span>
        <span class="setting-hint">выбор источника</span>
      </div>
      <select id="device-select" onchange="onDeviceChange()">
        <option value="">— нажмите «Начать» для загрузки —</option>
      </select>
    </div>

    <div class="sep"></div>

    <div class="setting-row">
      <div class="setting-head">
        <span class="setting-name">Усиление (Gain)</span>
        <span class="setting-val" id="gain-val">1.0×</span>
      </div>
      <input type="range" id="gain-slider" min="0.1" max="5" step="0.1" value="1"
             oninput="onGainChange(this.value)">
      <span class="setting-hint">Программное усиление сигнала. >1 — громче, <1 — тише.</span>
    </div>

    <div class="setting-row">
      <div class="setting-head">
        <span class="setting-name">Шумовой порог (Gate)</span>
        <span class="setting-val" id="gate-val">−∞</span>
      </div>
      <input type="range" id="gate-slider" min="-80" max="-10" step="1" value="-80"
             oninput="onGateChange(this.value)">
      <span class="setting-hint">Сигнал ниже порога заглушается. Помогает убрать фоновый шум.</span>
    </div>

    <div class="sep"></div>

    <div class="setting-row">
      <div class="setting-head">
        <span class="setting-name">Шумоподавление браузера</span>
      </div>
      <div class="toggle-group">
        <button class="toggle-btn on" id="btn-ns"    onclick="toggleConstraint('ns')">Вкл</button>
        <button class="toggle-btn"   id="btn-ns-off" onclick="toggleConstraintOff('ns')">Выкл</button>
      </div>
      <span class="setting-hint">noiseSuppression — встроенный фильтр браузера. Для музыки лучше выключить.</span>
    </div>

    <div class="setting-row">
      <div class="setting-head">
        <span class="setting-name">Эхоподавление (AEC)</span>
      </div>
      <div class="toggle-group">
        <button class="toggle-btn on" id="btn-ec"    onclick="toggleConstraint('ec')">Вкл</button>
        <button class="toggle-btn"   id="btn-ec-off" onclick="toggleConstraintOff('ec')">Выкл</button>
      </div>
      <span class="setting-hint">echoCancellation — убирает эхо от колонок.</span>
    </div>

    <div class="setting-row">
      <div class="setting-head">
        <span class="setting-name">Авто-усиление (AGC)</span>
      </div>
      <div class="toggle-group">
        <button class="toggle-btn"    id="btn-agc"     onclick="toggleConstraint('agc')">Вкл</button>
        <button class="toggle-btn on" id="btn-agc-off" onclick="toggleConstraintOff('agc')">Выкл</button>
      </div>
      <span class="setting-hint">autoGainControl — мешает ручному Gain, лучше выключить.</span>
    </div>

    <div class="sep"></div>

    <div class="setting-row">
      <div class="setting-head">
        <span class="setting-name">Частота дискретизации</span>
        <span class="setting-val" id="sr-val">48 000 Гц</span>
      </div>
      <input type="range" id="sr-slider" min="8000" max="48000" step="8000" value="48000"
             oninput="onSrChange(this.value)">
      <span class="setting-hint">Выше = лучше качество, больше трафик. 16 000 достаточно для речи.</span>
    </div>

    <div class="setting-row">
      <div class="setting-head">
        <span class="setting-name">Каналы</span>
      </div>
      <div class="toggle-group">
        <button class="toggle-btn on" id="btn-mono"   onclick="setChannels(1)">Моно</button>
        <button class="toggle-btn"   id="btn-stereo"  onclick="setChannels(2)">Стерео</button>
      </div>
      <span class="setting-hint">Моно вдвое меньше данных. Стерео нужно редко для речи.</span>
    </div>

  </div>
</div>

<div class="card">
  <div class="card-title">Сохранённые файлы</div>
  <div id="file-log"></div>
</div>

<script>
let ws = null, mediaRecorder = null, audioCtx = null;
let analyser = null, gainNode = null, stream = null;
let animId = null, segTimer = null, totalTimer = null;
let isRecording = false;
let totalSecs = 0, segSecs = 0, fileCount = 0;
const SEG = 120;

let cfg = {
  deviceId: '', gain: 1.0, gateDb: -80,
  ns: true, ec: true, agc: false,
  sampleRate: 48000, channels: 1,
};

const dot         = document.getElementById('dot');
const statusTxt   = document.getElementById('status-text');
const segFill     = document.getElementById('seg-fill');
const segTime     = document.getElementById('seg-time');
const totalTimeEl = document.getElementById('total-time');
const fileCountEl = document.getElementById('file-count');
const mainBtn     = document.getElementById('main-btn');
const vizIdle     = document.getElementById('viz-idle');
const canvas      = document.getElementById('viz');
const ctx2d       = canvas.getContext('2d');
const vuFill      = document.getElementById('vu-fill');
const vuVal       = document.getElementById('vu-val');

function setStatus(cls, txt) { dot.className = 'dot ' + cls; statusTxt.textContent = txt; }
function fmt(s) { return `${Math.floor(s/60)}:${String(s%60).padStart(2,'0')}`; }
function updateSegProgress() {
  const pct = (segSecs / SEG) * 100;
  segFill.style.width = pct + '%';
  segFill.style.background = pct > 80 ? 'var(--green)' : 'var(--accent)';
  segTime.textContent = `${fmt(segSecs)} / 2:00`;
}

function onGainChange(v) {
  cfg.gain = parseFloat(v);
  document.getElementById('gain-val').textContent = cfg.gain.toFixed(1) + '×';
  if (gainNode) gainNode.gain.setTargetAtTime(cfg.gain, audioCtx.currentTime, 0.01);
}
function onGateChange(v) {
  cfg.gateDb = parseInt(v);
  document.getElementById('gate-val').textContent = cfg.gateDb <= -80 ? '−∞' : cfg.gateDb + ' дБ';
}
function onSrChange(v) {
  cfg.sampleRate = parseInt(v);
  document.getElementById('sr-val').textContent = cfg.sampleRate.toLocaleString('ru') + ' Гц';
}
function setChannels(n) {
  cfg.channels = n;
  document.getElementById('btn-mono').classList.toggle('on', n === 1);
  document.getElementById('btn-stereo').classList.toggle('on', n === 2);
}
function toggleConstraint(key) {
  const map = { ns:['btn-ns','btn-ns-off'], ec:['btn-ec','btn-ec-off'], agc:['btn-agc','btn-agc-off'] };
  cfg[key] = true;
  document.getElementById(map[key][0]).classList.add('on');
  document.getElementById(map[key][1]).classList.remove('on');
}
function toggleConstraintOff(key) {
  const map = { ns:['btn-ns','btn-ns-off'], ec:['btn-ec','btn-ec-off'], agc:['btn-agc','btn-agc-off'] };
  cfg[key] = false;
  document.getElementById(map[key][0]).classList.remove('on');
  document.getElementById(map[key][1]).classList.add('on');
}
function onDeviceChange() {
  cfg.deviceId = document.getElementById('device-select').value;
  if (isRecording) restartStream();
}

async function loadDevices() {
  try {
    const devices = await navigator.mediaDevices.enumerateDevices();
    const sel = document.getElementById('device-select');
    sel.innerHTML = '';
    devices.filter(d => d.kind === 'audioinput').forEach(d => {
      const opt = document.createElement('option');
      opt.value = d.deviceId;
      opt.textContent = d.label || `Микрофон (${d.deviceId.slice(0,8)}...)`;
      if (d.deviceId === cfg.deviceId) opt.selected = true;
      sel.appendChild(opt);
    });
    if (!cfg.deviceId && sel.options.length > 0) cfg.deviceId = sel.options[0].value;
  } catch(e) { console.warn('enumerateDevices:', e); }
}

async function buildAudioGraph(rawStream) {
  audioCtx = new AudioContext({ sampleRate: cfg.sampleRate });
  const src = audioCtx.createMediaStreamSource(rawStream);
  gainNode = audioCtx.createGain();
  gainNode.gain.value = cfg.gain;
  analyser = audioCtx.createAnalyser();
  analyser.fftSize = 512;
  analyser.smoothingTimeConstant = 0.6;
  const gateProc = audioCtx.createScriptProcessor(2048, cfg.channels, cfg.channels);
  gateProc.onaudioprocess = (ev) => {
    const gateLinear = cfg.gateDb <= -80 ? 0 : Math.pow(10, cfg.gateDb / 20);
    for (let ch = 0; ch < ev.inputBuffer.numberOfChannels; ch++) {
      const inData = ev.inputBuffer.getChannelData(ch);
      const outData = ev.outputBuffer.getChannelData(ch);
      let rms = 0;
      for (let i = 0; i < inData.length; i++) rms += inData[i] * inData[i];
      rms = Math.sqrt(rms / inData.length);
      if (gateLinear === 0 || rms >= gateLinear) outData.set(inData);
      else for (let i = 0; i < inData.length; i++) outData[i] = 0;
    }
  };
  const dest = audioCtx.createMediaStreamDestination();
  src.connect(gainNode);
  gainNode.connect(gateProc);
  gateProc.connect(analyser);
  analyser.connect(dest);
  return dest.stream;
}

async function toggleRecording() {
  if (isRecording) stopRecording(); else await startRecording();
}

async function startRecording() {
  try {
    setStatus('connecting', 'Запрос микрофона...');
    mainBtn.disabled = true;
    await doStart();
    mainBtn.disabled = false;
    mainBtn.className = 'btn btn-stop';
    mainBtn.textContent = '■ Остановить запись';
  } catch(err) {
    console.error(err);
    setStatus('', 'Ошибка: ' + err.message);
    mainBtn.disabled = false;
  }
}

async function doStart() {
  // Проверяем доступность API до обращения к нему
  if (!navigator.mediaDevices?.getUserMedia) {
    throw new Error('Микрофон недоступен — откройте страницу по HTTPS');
  }

  const constraints = {
    audio: {
      deviceId: cfg.deviceId ? { exact: cfg.deviceId } : undefined,
      noiseSuppression: cfg.ns, echoCancellation: cfg.ec,
      autoGainControl: cfg.agc, channelCount: cfg.channels, sampleRate: cfg.sampleRate,
    }
  };

  stream = await navigator.mediaDevices.getUserMedia(constraints);
  await loadDevices();
  document.getElementById('device-select').disabled = false;

  const processedStream = await buildAudioGraph(stream);
  vizIdle.style.display = 'none';
  drawViz();

  // wss:// потому что сервер теперь HTTPS
  ws = new WebSocket(`wss://${location.host}/ws/audio`);
  ws.binaryType = 'arraybuffer';

  await new Promise((res, rej) => {
    ws.onopen  = res;
    ws.onerror = () => rej(new Error('WebSocket не подключился'));
  });

  setStatus('recording', 'Запись идёт...');
  startMediaRecorder(processedStream);

  ws.onmessage = (e) => {
    try {
      const msg = JSON.parse(e.data);
      if (msg.type === 'file_saved') {
        fileCount++;
        fileCountEl.textContent = fileCount;
        addFileLog(msg.filename, msg.size_kb);
        segSecs = 0;
        updateSegProgress();
      }
    } catch(_) {}
  };
  ws.onclose = () => { if (isRecording) setStatus('connecting', 'Соединение прервано'); };
  // Игнорируем ошибку WS во время штатного завершения записи —
  // Safari рвёт соединение раньше чем мы успеваем закрыть его сами.
  ws.onerror = () => { if (isRecording) setStatus('', 'Ошибка WebSocket'); };

  totalTimer = setInterval(() => { totalSecs++; totalTimeEl.textContent = fmt(totalSecs); }, 1000);
  segTimer   = setInterval(() => { segSecs++;   updateSegProgress(); }, 1000);
  isRecording = true;
}

function startMediaRecorder(src) {
  const mimeType = MediaRecorder.isTypeSupported('audio/webm;codecs=opus')
    ? 'audio/webm;codecs=opus'
    : MediaRecorder.isTypeSupported('audio/webm') ? 'audio/webm' : '';
  mediaRecorder = new MediaRecorder(src, mimeType ? { mimeType } : {});
  mediaRecorder.ondataavailable = (e) => {
    if (e.data?.size > 0 && ws?.readyState === WebSocket.OPEN)
      e.data.arrayBuffer().then(buf => ws.send(buf));
  };
  mediaRecorder.start(250);
}

async function restartStream() {
  if (mediaRecorder?.state !== 'inactive') mediaRecorder.stop();
  if (stream) stream.getTracks().forEach(t => t.stop());
  if (audioCtx) { await audioCtx.close(); audioCtx = null; gainNode = null; analyser = null; }
  if (animId) cancelAnimationFrame(animId);
  try {
    const processedStream = await buildAudioGraph(
      await navigator.mediaDevices.getUserMedia({
        audio: {
          deviceId: cfg.deviceId ? { exact: cfg.deviceId } : undefined,
          noiseSuppression: cfg.ns, echoCancellation: cfg.ec,
          autoGainControl: cfg.agc, channelCount: cfg.channels, sampleRate: cfg.sampleRate,
        }
      })
    );
    vizIdle.style.display = 'none';
    drawViz();
    startMediaRecorder(processedStream);
    setStatus('recording', 'Запись идёт...');
  } catch(e) {
    setStatus('', 'Ошибка смены устройства: ' + e.message);
  }
}

function stopRecording() {
  isRecording = false;
  clearInterval(totalTimer); clearInterval(segTimer);
  if (animId) cancelAnimationFrame(animId);

  setStatus('', 'Завершение записи...');
  mainBtn.disabled = true;

  function cleanup() {
    if (stream) stream.getTracks().forEach(t => t.stop());
    if (audioCtx) audioCtx.close();
    ws = null; mediaRecorder = null; stream = null; audioCtx = null;
    gainNode = null; analyser = null;
    vizIdle.style.display = 'flex';
    vuFill.style.width = '0%';
    vuVal.textContent = '— дБ';
    document.getElementById('device-select').disabled = true;
    setStatus('', 'Запись остановлена');
    mainBtn.className = 'btn btn-start';
    mainBtn.textContent = '▶ Начать запись';
    mainBtn.disabled = false;
  }

  if (mediaRecorder && mediaRecorder.state !== 'inactive') {
    // Ждём последний ondataavailable после stop(),
    // только потом закрываем WebSocket — иначе последний сегмент теряется.
    mediaRecorder.addEventListener('stop', () => {
      // Небольшая пауза чтобы последний бинарный чанк успел уйти по WS
      setTimeout(() => {
        if (ws) ws.close();
        cleanup();
      }, 300);
    }, { once: true });
    mediaRecorder.stop();
  } else {
    if (ws) ws.close();
    cleanup();
  }
}

function addFileLog(fname, sizeKb) {
  const el = document.createElement('div');
  el.className = 'log-entry';
  el.innerHTML = `<span class="icon">✓</span><span class="fname">${fname}</span><span class="size">${sizeKb} KB</span>`;
  document.getElementById('file-log').prepend(el);
}

function drawViz() {
  if (!analyser) return;
  animId = requestAnimationFrame(drawViz);
  const W = canvas.offsetWidth, H = canvas.offsetHeight;
  canvas.width = W; canvas.height = H;
  const bufLen = analyser.frequencyBinCount;
  const freq   = new Uint8Array(bufLen);
  const time   = new Float32Array(analyser.fftSize);
  analyser.getByteFrequencyData(freq);
  analyser.getFloatTimeDomainData(time);
  ctx2d.clearRect(0, 0, W, H);
  const barW = W / bufLen * 2.5;
  let x = 0;
  for (let i = 0; i < bufLen; i++) {
    const v = freq[i] / 255, h = v * H;
    ctx2d.fillStyle = `hsla(${200 + v*40}, 80%, ${40+v*30}%, ${0.4+v*0.6})`;
    ctx2d.fillRect(x, H - h, barW - 1, h);
    x += barW + 1;
    if (x > W) break;
  }
  let rms = 0;
  for (let i = 0; i < time.length; i++) rms += time[i] * time[i];
  rms = Math.sqrt(rms / time.length);
  const db = rms > 0 ? 20 * Math.log10(rms) : -Infinity;
  const pct = Math.max(0, Math.min(100, (db + 60) / 60 * 100));
  vuFill.style.width = pct + '%';
  vuFill.style.background = pct > 85 ? 'var(--danger)' : pct > 65 ? 'var(--yellow)' : 'var(--green)';
  vuVal.textContent = isFinite(db) ? db.toFixed(1) + ' дБ' : '−∞';
}
</script>
</body>
</html>
"#;

// ─── Состояние воркера ────────────────────────────────────────────────────────

// Магические байты начала EBML-документа (WebM/MKV)
const EBML_MAGIC: [u8; 4] = [0x1A, 0x45, 0xDF, 0xA3];

struct RecorderState {
    buffer:      Vec<u8>,
    started_at:  std::time::Instant,
    file_index:  u32,
    session_id:  String,
    /// EBML-заголовок первого WebM чанка.
    /// MediaRecorder шлёт его только один раз — в начале стрима.
    /// Без него все последующие сегменты невалидны.
    /// Сохраняем и prepend-им к каждому новому сегменту.
    webm_header: Vec<u8>,
}

impl RecorderState {
    fn new(session_id: String) -> Self {
        Self {
            buffer:      Vec::new(),
            started_at:  std::time::Instant::now(),
            file_index:  0,
            session_id,
            webm_header: Vec::new(),
        }
    }

    fn starts_with_ebml(data: &[u8]) -> bool {
        data.len() >= 4 && data[..4] == EBML_MAGIC
    }
}

// ─── Хендлеры ────────────────────────────────────────────────────────────────

async fn index() -> impl IntoResponse { Html(HTML) }

async fn ws_handler(ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(handle_audio_socket)
}

async fn handle_audio_socket(socket: WebSocket) {
    let ts = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
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
            Ok(Some(Ok(m)))  => m,
            Ok(Some(Err(e))) => { error!("WS: {e}"); break; }
            Ok(None)         => { info!("Клиент отключился"); break; }
            Err(_)           => { error!("Таймаут"); break; }
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
                        let json = format!(r#"{{"type":"file_saved","filename":"{fname}","size_kb":{kb}}}"#);
                        if sender.send(Message::Text(json)).await.is_err() { break; }
                    }
                }
            }
            Message::Close(_) => { info!("Close frame"); break; }
            Message::Ping(p)  => { let _ = sender.send(Message::Pong(p)).await; }
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
    if st.buffer.is_empty() { return None; }

    st.file_index += 1;
    let filename = format!("{}/{}_seg{:04}.webm", OUTPUT_DIR, st.session_id, st.file_index);
    let data = std::mem::take(&mut st.buffer);

    // Если сегмент не начинается с EBML-заголовка — prepend сохранённый заголовок.
    // Это критично: MediaRecorder шлёт заголовок только в первом чанке,
    // без него ffmpeg/whisper не могут прочитать файл.
    let write_data: Vec<u8> = if !RecorderState::starts_with_ebml(&data) && !st.webm_header.is_empty() {
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
        Err(e) => { error!("Создание {filename}: {e}"); return None; }
    }

    st.started_at = std::time::Instant::now();
    let short = PathBuf::from(&filename).file_name()
        .and_then(|n| n.to_str()).unwrap_or(&filename).to_string();
    Some((short, size_kb))
}

// ─── Transcripts API ─────────────────────────────────────────────────────────

#[derive(Serialize)]
struct Segment {
    filename: String,
    index: u32,
    text: Option<String>,   // None = ещё не транскрибировано
    has_audio: bool,
}

#[derive(Serialize)]
struct Session {
    id: String,             // rec_XXXXXXXXXX
    segments: Vec<Segment>,
    total_text: String,     // весь текст сессии одной строкой
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
        if !name.ends_with(".webm") { continue; }

        // rec_1781188527_seg0001.webm  →  session_id = "rec_1781188527"
        let Some(session_id) = name
            .rsplitn(2, "_seg")
            .last()
            .map(|s| s.to_string())
        else { continue };

        // индекс сегмента
        let index: u32 = name
            .trim_end_matches(".webm")
            .rsplitn(2, "_seg")
            .next()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        // читаем .txt если есть
        let txt_path = format!("{}/{}", OUTPUT_DIR, name.replace(".webm", ".txt"));
        let text = tokio::fs::read_to_string(&txt_path).await.ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        sessions.entry(session_id).or_default().push(Segment {
            filename: name,
            index,
            text,
            has_audio: true,
        });
    }

    // Сортируем сегменты внутри сессий, собираем итог
    let result: Vec<Session> = sessions
        .into_iter()
        .rev()                          // новые сессии первыми
        .map(|(id, mut segs)| {
            segs.sort_by_key(|s| s.index);
            let total_text = segs.iter()
                .filter_map(|s| s.text.as_deref())
                .collect::<Vec<_>>()
                .join("\n");
            Session { id, segments: segs, total_text }
        })
        .collect();

    Json(result)
}

// HTML страница транскриптов (SPA — данные грузит через /api/transcripts)
const TRANSCRIPTS_HTML: &str = r#"<!DOCTYPE html>
<html lang="ru">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>Транскрипты</title>
<style>
  :root {
    --bg:       #0d0f14;
    --surface:  #151820;
    --border:   #1e2330;
    --accent:   #4f8ef7;
    --text:     #c8d0e0;
    --text-dim: #5a6480;
    --green:    #3ecf8e;
    --yellow:   #f7c44f;
    --mono: 'JetBrains Mono', 'Fira Code', 'Courier New', monospace;
  }
  * { box-sizing: border-box; margin: 0; padding: 0; }
  body {
    background: var(--bg); color: var(--text);
    font-family: var(--mono); min-height: 100vh;
    padding: 24px 16px 64px;
    display: flex; flex-direction: column; align-items: center; gap: 12px;
  }

  /* ── шапка ── */
  .topbar {
    width: 100%; max-width: 720px;
    display: flex; justify-content: space-between; align-items: center;
    padding-bottom: 12px; border-bottom: 1px solid var(--border);
    margin-bottom: 4px;
  }
  .topbar-title { font-size: 18px; font-weight: 700; }
  .topbar-sub   { font-size: 11px; color: var(--text-dim); margin-top: 2px; }
  .nav-link {
    font-size: 12px; color: var(--accent); text-decoration: none;
    padding: 6px 14px; border: 1px solid var(--accent);
    border-radius: 6px; transition: background .15s;
  }
  .nav-link:hover { background: #1e3260; }

  /* ── сессия ── */
  .session {
    width: 100%; max-width: 720px;
    background: var(--surface); border: 1px solid var(--border);
    border-radius: 12px; overflow: hidden;
  }
  .session-header {
    display: flex; justify-content: space-between; align-items: center;
    padding: 14px 18px; border-bottom: 1px solid var(--border);
    cursor: pointer; user-select: none;
    transition: background .15s;
  }
  .session-header:hover { background: #1a1e28; }
  .session-id   { font-size: 13px; font-weight: 600; }
  .session-meta { font-size: 11px; color: var(--text-dim); margin-top: 2px; }
  .session-actions { display: flex; gap: 8px; align-items: center; }
  .chevron { font-size: 12px; color: var(--text-dim); transition: transform .2s; }
  .chevron.open { transform: rotate(180deg); }

  /* ── кнопка копирования ── */
  .copy-btn {
    padding: 5px 12px; border-radius: 5px;
    border: 1px solid var(--border); background: var(--bg);
    color: var(--text-dim); font-family: var(--mono); font-size: 11px;
    cursor: pointer; transition: all .15s; white-space: nowrap;
  }
  .copy-btn:hover  { border-color: var(--accent); color: var(--accent); }
  .copy-btn.copied { border-color: var(--green);  color: var(--green); }

  /* ── тело сессии ── */
  .session-body { display: none; flex-direction: column; }
  .session-body.open { display: flex; }

  /* ── сегмент ── */
  .segment {
    padding: 14px 18px;
    border-bottom: 1px solid var(--border);
  }
  .segment:last-child { border-bottom: none; }
  .seg-header {
    display: flex; justify-content: space-between; align-items: center;
    margin-bottom: 8px;
  }
  .seg-label { font-size: 11px; color: var(--text-dim); letter-spacing: .1em; }
  .seg-status-ok      { font-size: 11px; color: var(--green); }
  .seg-status-pending { font-size: 11px; color: var(--yellow); }

  .seg-text {
    font-size: 13px; line-height: 1.7;
    white-space: pre-wrap; word-break: break-word;
    color: var(--text);
  }
  .seg-text.empty { color: var(--text-dim); font-style: italic; }

  /* ── пустое состояние ── */
  .empty {
    color: var(--text-dim); font-size: 13px; text-align: center;
    padding: 48px 0;
  }

  /* ── обновление ── */
  .refresh-bar {
    width: 100%; max-width: 720px;
    display: flex; justify-content: flex-end; gap: 10px; align-items: center;
  }
  .refresh-hint { font-size: 11px; color: var(--text-dim); }
  .refresh-btn {
    padding: 5px 14px; border-radius: 5px;
    border: 1px solid var(--border); background: var(--bg);
    color: var(--text-dim); font-family: var(--mono); font-size: 11px;
    cursor: pointer; transition: all .15s;
  }
  .refresh-btn:hover { border-color: var(--accent); color: var(--accent); }
</style>
</head>
<body>

<div class="topbar">
  <div>
    <div class="topbar-title">Транскрипты</div>
    <div class="topbar-sub">Тайский текст по сессиям</div>
  </div>
  <a href="/" class="nav-link">⬅ Запись</a>
</div>

<div class="refresh-bar">
  <span class="refresh-hint" id="last-update"></span>
  <button class="refresh-btn" onclick="load()">↻ Обновить</button>
</div>

<div id="root" style="width:100%;max-width:720px;display:flex;flex-direction:column;gap:12px">
  <div class="empty">Загрузка...</div>
</div>

<script>
async function load() {
  const root = document.getElementById('root');
  try {
    const r = await fetch('/api/transcripts');
    const sessions = await r.json();

    document.getElementById('last-update').textContent =
      'Обновлено: ' + new Date().toLocaleTimeString('ru');

    if (!sessions.length) {
      root.innerHTML = '<div class="empty">Нет записей. Сначала запишите аудио.</div>';
      return;
    }

    root.innerHTML = '';
    sessions.forEach((sess, si) => {
      const totalSegs  = sess.segments.length;
      const doneSegs   = sess.segments.filter(s => s.text).length;
      const hasAnyText = !!sess.total_text.trim();

      const el = document.createElement('div');
      el.className = 'session';
      el.innerHTML = `
        <div class="session-header" onclick="toggleSession(${si})">
          <div>
            <div class="session-id">${sess.id}</div>
            <div class="session-meta">${totalSegs} сегм. · ${doneSegs} транскрибировано</div>
          </div>
          <div class="session-actions">
            ${hasAnyText ? `<button class="copy-btn" onclick="copyText(event, ${JSON.stringify(sess.total_text)})">
              Копировать всё
            </button>` : ''}
            <span class="chevron ${si === 0 ? 'open' : ''}" id="chev-${si}">▼</span>
          </div>
        </div>
        <div class="session-body ${si === 0 ? 'open' : ''}" id="body-${si}">
          ${sess.segments.map(seg => `
            <div class="segment">
              <div class="seg-header">
                <span class="seg-label">SEG ${String(seg.index).padStart(4,'0')} · ${seg.filename}</span>
                <div style="display:flex;gap:8px;align-items:center">
                  ${seg.text
                    ? `<span class="seg-status-ok">✓ готово</span>
                       <button class="copy-btn" onclick="copyText(event, ${JSON.stringify(seg.text)})">Копировать</button>`
                    : `<span class="seg-status-pending">⏳ обрабатывается</span>`
                  }
                </div>
              </div>
              ${seg.text
                ? `<div class="seg-text">${escHtml(seg.text)}</div>`
                : `<div class="seg-text empty">Транскрипт ещё не готов</div>`
              }
            </div>
          `).join('')}
        </div>
      `;
      root.appendChild(el);
    });
  } catch(e) {
    root.innerHTML = `<div class="empty">Ошибка загрузки: ${e.message}</div>`;
  }
}

function toggleSession(si) {
  const body = document.getElementById('body-' + si);
  const chev = document.getElementById('chev-' + si);
  const open = body.classList.toggle('open');
  chev.classList.toggle('open', open);
}

async function copyText(e, text) {
  e.stopPropagation();
  const btn = e.currentTarget;
  try {
    await navigator.clipboard.writeText(text);
    btn.classList.add('copied');
    btn.textContent = '✓ Скопировано';
    setTimeout(() => {
      btn.classList.remove('copied');
      btn.textContent = btn.dataset.label || (text.includes('\n') ? 'Копировать всё' : 'Копировать');
    }, 2000);
  } catch(_) {
    // Fallback для Safari
    const ta = document.createElement('textarea');
    ta.value = text; ta.style.position = 'fixed'; ta.style.opacity = '0';
    document.body.appendChild(ta); ta.select();
    document.execCommand('copy');
    document.body.removeChild(ta);
    btn.classList.add('copied');
    btn.textContent = '✓ Скопировано';
    setTimeout(() => { btn.classList.remove('copied'); btn.textContent = 'Копировать'; }, 2000);
  }
}

function escHtml(s) {
  return s.replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;');
}

// Автообновление каждые 15 секунд
load();
setInterval(load, 15000);
</script>
</body>
</html>
"#;

async fn transcripts_page() -> impl IntoResponse { Html(TRANSCRIPTS_HTML) }

// ─── main ────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_target(false)
        .with_max_level(tracing::Level::INFO)
        .init();

    let app = Router::new()
        .route("/",                get(index))
        .route("/ws/audio",        get(ws_handler))
        .route("/transcripts",     get(transcripts_page))
        .route("/api/transcripts", get(api_transcripts));

    // Сертификат и ключ генерируются build.rs автоматически при сборке
    let tls = RustlsConfig::from_pem_file("cert.pem", "key.pem")
        .await
        .expect("Не удалось загрузить TLS-сертификат. Запусти `cargo build` для генерации.");

    let addr = "0.0.0.0:3005".parse().unwrap();
    info!("Сервер: https://{addr}  →  записи в ./{OUTPUT_DIR}/");
    info!("На телефоне: откройте https://<IP>:3005 и примите предупреждение о сертификате");

    axum_server::bind_rustls(addr, tls)
        .serve(app.into_make_service())
        .await
        .unwrap();
}
