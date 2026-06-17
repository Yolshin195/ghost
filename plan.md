╔═══════════════════════════════════════════════════════════════════════════════╗
║           ТРАНСКРИБАТОР ТАЙСКОГО АУДИО — ПОЛНАЯ АРХИТЕКТУРА               ║
╚═══════════════════════════════════════════════════════════════════════════════╝

📋 ФАЗА 1: ИНФРАСТРУКТУРА (✅ ГОТОВО)
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

Структура проекта:
```
project/
├── src/
│   ├── lib.rs                  ← экспортирует все модули (pub use)
│   ├── model.rs                ✓ Модели данных (Session, Segment, Utterance)
│   ├── repository.rs           ✓ Слой доступа к БД (SQLite + sqlx)
│   ├── service.rs              ✓ НОВЫЙ — бизнес-логика (SessionService, SegmentService, etc.)
│   ├── parse.rs                ✓ НОВЫЙ — парсинг Whisper .txt (временные метки)
│   ├── database.rs             ✓ НОВЫЙ — инит БД, миграции
│   └── handler.rs              ✓ НОВЫЙ — HTTP endpoints для API
├── bin/
│   ├── server.rs               ✓ НОВЫЙ — основной сервер (запись + API)
│   └── ingest.rs               ✓ НОВЫЙ — утилита загрузки данных
├── Cargo.toml                  ✓ НОВЫЙ — зависимости и конфиг
└── recordings/
    ├── rec_1781616471_seg0001.webm
    ├── rec_1781616471_seg0001.txt
    ├── cli_1781598604_seg0001.wav
    └── cli_1781598604_seg0001.txt
```

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

📊 СТРУКТУРА БД (SQLite)
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

sessions
├── id (INTEGER PRIMARY KEY)
├── slug (TEXT UNIQUE)          — rec_1234567890, cli_1234567890
├── created_at (INTEGER)        — Unix timestamp
└── note (TEXT)                 — заметка оператора

segments
├── id (INTEGER PRIMARY KEY)
├── session_id (FK)
├── idx (INTEGER)               — порядковый номер (1, 2, 3, …)
├── filename (TEXT UNIQUE)      — rec_XXX_seg0001.webm
├── size_bytes (INTEGER)
├── recorded_at (INTEGER)       — Unix timestamp записи файла
├── transcription_status (TEXT) — pending / processing / done / error
└── error_message (TEXT)        — при ошибке

speakers
├── id (INTEGER PRIMARY KEY)
├── session_id (FK)
├── label (TEXT)                — Оператор, Клиент, Speaker 1
└── color (TEXT)                — CSS hex (#4f8ef7)

utterances
├── id (INTEGER PRIMARY KEY)
├── segment_id (FK)
├── idx (INTEGER)               — 0-based (как у Whisper)
├── start_sec (REAL)            — [MM:SS] → секунды
├── end_sec (REAL)              — до следующей реплики
├── text_thai (TEXT)            — оригинальный текст из Whisper
├── text_ru (TEXT)              — перевод (вводится вручную)
├── speaker_id (FK)             — для диаризации (пока NULL)
└── confidence (REAL)           — [0.0, 1.0] (расширение в будущем)

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

🔄 ФАЗА 1.5: ЗАГРУЗКА ДАННЫХ В БД (bin/ingest.rs)
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

Утилита сканирует recordings/ и заполняет БД.

Использование:
  cargo run --bin ingest -- --db ./transcripts.db --input ./recordings
  cargo run --bin ingest -- --watch   # следить за новыми файлами

Логика:
  1. Сканирует recordings/ ищет *.webm, *.wav
  2. Для каждого аудио ищет соответствующий .txt (Whisper-результат)
  3. Парсит имя файла: rec_TIMESTAMP_segINDEX → (session_slug, segment_index)
  4. Проверяет БД: уже ли есть этот сегмент?
     - Если да → пропускает (идемпотентно)
     - Если нет → создаёт Session/Segment
  5. Парсит .txt файл → список реплик с временными метками
     Формат: [MM:SS] текст реплики
  6. Создаёт Utterance'ы для каждой реплики
  7. Выставляет статус сегмента = Done

Парсинг имени файла:
  rec_1781616471_seg0001.webm  →  (session_slug: "rec_1781616471", idx: 1)
  cli_1781598604_seg0015.wav   →  (session_slug: "cli_1781598604", idx: 15)

Парсинг .txt файла (parse.rs):
  Входной формат:
    [00:00] เอาอ่ะ แล้วยังไงอ่ะแล้ว...
    [00:38] ยาตัวเองโดยการที่ไป...
    [01:09] กูก็เป็นแต่ก็น้อยไง...

  Regex: ^\[(\d{1,2}):(\d{2})\]\s+(.+)$
  
  Вычисляет:
    start_sec = MM*60 + SS
    end_sec = next_timestamp (или примерная длиность последней реплики)
    text_thai = захваченный текст

Пример результата:
  ✓ Сегмент успешно загружен с 4 репликами
  ✓ Session rec_1781616471 создана/получена
  ✓ Segment seg0001 создан (size: 256 KB)
  ✓ 4 Utterance'ы вставлены в БД

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

🌐 API ENDPOINTS (handler.rs + server.rs)
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

Sessions:
  GET  /api/sessions             — список всех сессий (с счётчиками)
  GET  /api/sessions/:id         — полная сессия со всеми сегментами/репликами
  DELETE /api/sessions/:id       — удалить сессию и всё связанное

Segments:
  GET  /api/segments/:id         — сегмент с репликами и спикерами

Utterances (реплики):
  PATCH /api/utterances/:id/translation  — обновить перевод на русский
  POST /api/utterances/:id/speaker       — назначить спикера (для диаризации)

Health:
  GET  /health                   — проверка живости сервера

WebSocket (запись):
  WS   /ws/audio                 — приём аудио из браузера (MediaRecorder)
  WS   /ws/audio-pcm             — приём PCM f32 из консольного клиента

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

📱 ФАЗА 2: WEB UI (HTML страница для просмотра и редактирования) 
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

Страница: /transcripts.html (SPA на ванильном JS)

Визуал:
  ┌─────────────────────────────────────────────────┐
  │  Транскрипты  ↺                                │
  └─────────────────────────────────────────────────┘

  ┌─ Session: rec_1781616471 (5 сегм. · 3 переведено) ──┐  ▼ раскрыть
  │                                                      │
  │  SEG0001 · rec_1781616471_seg0001.txt               │
  │  [00:00] เอาอ่ะ แล้วยังไงอ่ะ... | 📝 Добавить      │
  │  [00:38] ยาตัวเองโดยการที่ไป  | ✓ Переведено      │
  │  [01:09] กูก็เป็นแต่ก็น้อยไง     | 📝 Добавить      │
  │  [01:39] มันนานแล้วเหมือนกัน   | ✓ Переведено      │
  │                                                      │
  │  SEG0002 · rec_1781616471_seg0002.txt               │
  │  ...                                                 │
  │                                                      │
  │  📋 Копировать ВСЁ (тайский + русский)             │
  └──────────────────────────────────────────────────────┘

Функционал:
  ✓ Список всех сессий с счётчиками
  ✓ Раскрытие сессии → все сегменты + реплики
  ✓ Каждая реплика: [MM:SS] text_thai | тайский | русский
  ✓ Inline редактирование перевода (click → textarea → save)
  ✓ Auto-save при изменении (PATCH /api/utterances/:id/translation)
  ✓ Копирование весь текст сессии одной кнопкой
  ✓ Удаление сессии
  ✓ Auto-refresh каждые 15 секунд (для слежения за ingest.rs)

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

⚙️ КОД: СЛОИ АРХИТЕКТУРЫ
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

1. MODEL (model.rs) ✓
   ├── Session, Segment, Utterance, Speaker (основные entity)
   ├── NewSession, NewSegment, NewUtterance (DTO для создания)
   ├── SessionDetail, SegmentWithUtterances (composite DTO для API)
   └── TranscriptionStatus enum (pending / processing / done / error)

2. REPOSITORY (repository.rs) ✓
   ├── Repository::open() — открыть БД с миграциями
   ├── create_session(), get_session(), list_sessions()
   ├── create_segment(), get_segment_by_filename()
   ├── replace_utterances_for_segment() — batch-вставка с транзакцией
   ├── update_translation() — сохранить перевод
   ├── get_session_detail() — full JOIN для полной сессии
   └── set_transcription_status() — обновить статус обработки

3. SERVICE (service.rs) ✓
   ├── SessionService — управление сессиями (create, get, delete)
   ├── SegmentService — управление сегментами и их статусом
   ├── UtteranceService — обновление переводов и спикеров
   └── IngestService — главная: парсинг файлов + заполнение БД
       └── ingest_audio_with_transcript() — основной метод

4. PARSE (parse.rs) ✓
   ├── parse_whisper_txt() — парсит файл
   ├── parse_whisper_content() — парсит содержимое (для тестов)
   └── WhisperUtterance { start_sec, end_sec, text_thai }

5. DATABASE (database.rs) ✓
   └── init_db() — инициализирует SQLite с миграциями (WAL, NORMAL sync)

6. HANDLER (handler.rs) ✓
   ├── list_sessions() — GET /api/sessions
   ├── get_session() — GET /api/sessions/:id
   ├── delete_session() — DELETE /api/sessions/:id
   ├── get_segment() — GET /api/segments/:id
   ├── update_utterance_translation() — PATCH /api/utterances/:id/translation
   ├── assign_speaker_to_utterance() — POST /api/utterances/:id/speaker
   ├── ApiError, ApiResult — обработка ошибок
   └── SharedState { repo } — состояние для всех handlers

7. MAIN (bin/server.rs) ✓
   ├── WebSocket /ws/audio — приём аудио (текущий код из main.rs)
   └── API endpoints ← handler.rs

8. INGEST (bin/ingest.rs) ✓
   ├── Сканирует recordings/ собирает пары (audio, txt)
   ├── Обрабатывает каждый файл через IngestService
   └── --watch режим для автоматической обработки новых файлов

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

🚀 ИСПОЛЬЗОВАНИЕ
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

Шаг 1. Запустить сервер записи:
  cargo run --bin server

Шаг 2. Записать аудио в браузер / клиент:
  curl https://localhost:3005  # видим UI для записи

Шаг 3. Запустить Whisper (Python):
  python transcribe_thai_mac.py recordings/

Шаг 4. Загрузить данные в БД:
  cargo run --bin ingest -- --db ./transcripts.db --input ./recordings

Шаг 5. Посмотреть результаты в браузере:
  https://localhost:3005/transcripts.html

Шаг 6. Добавить переводы вручную:
  Кликнуть на реплику → редактировать → сохраняется автоматически

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

🧪 ТЕСТИРОВАНИЕ
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

Parse.rs имеет unit-тесты:
  cargo test parse

Парсинг проверяется:
  ✓ Пустые файлы
  ✓ Одна строка
  ✓ Несколько строк с правильным вычислением end_sec
  ✓ Пропуск пустых линий

Service.rs имеет unit-тест для парсинга имён файлов:
  cargo test service

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

📝 ЗАМЕТКИ
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

• Диаризация (многоспикер): заготовка в speaker_id (сейчас всегда NULL)
  Расширение: запустить pyannote отдельно, обновить speaker_id

• Перевод: вводится вручную через UI, сохраняется в text_ru

• Уверенность: расширение — Whisper может вернуть confidence
  (текущий format Whisper .txt не содержит, но структура готова)

• Идемпотентность: ingest.rs не перепроцессирует файлы
  Проверка: SELECT * FROM segments WHERE filename = ?

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

✅ ЧТО ГОТОВО
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

[✓] src/lib.rs              — модули, re-export
[✓] src/model.rs            — модели (было, расширения не требуются)
[✓] src/repository.rs       — репо (было, small fixes OK)
[✓] src/service.rs          — бизнес-логика
[✓] src/parse.rs            — парсинг Whisper .txt
[✓] src/database.rs         — инит БД
[✓] src/handler.rs          — API endpoints
[✓] bin/server.rs           — основной сервер
[✓] bin/ingest.rs           — утилита загрузки
[✓] Cargo.toml              — зависимости

[⏳] pages/transcripts.html   — СЛЕДУЮЩИЙ ШАГ (Фаза 2)

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━