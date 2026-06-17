╔════════════════════════════════════════════════════════════════════════════╗
║        ИНСТРУКЦИЯ: КАК ИНТЕГРИРОВАТЬ КОД В ТВОЙ ПРОЕКТ               ║
╚════════════════════════════════════════════════════════════════════════════╝

📥 ШАГ 1: ПОДГОТОВКА СТРУКТУРЫ
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

Если это новый проект, создаём структуру:

  mkdir -p src bin recordings pages
  touch Cargo.toml src/lib.rs

Если у тебя уже есть проект с main.rs — переименуй его:

  mv src/main.rs bin/server.rs

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

📄 ШАГ 2: КОПИРУЕМ НОВЫЕ ФАЙЛЫ
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

В папке /home/claude/ у нас есть:

  КОПИРОВАТЬ В src/:
  ├── lib.rs              → src/lib.rs
  ├── model.rs            → src/model.rs (ТЫ УЖЕ ИМЕЕШЬ)
  ├── repository.rs       → src/repository.rs (ТЫ УЖЕ ИМЕЕШЬ)
  ├── service.rs          → src/service.rs ✓ НОВЫЙ
  ├── parse.rs            → src/parse.rs ✓ НОВЫЙ
  ├── database.rs         → src/database.rs ✓ НОВЫЙ
  └── handler.rs          → src/handler.rs ✓ НОВЫЙ

  КОПИРОВАТЬ В bin/:
  ├── bin_server.rs       → bin/server.rs ✓ НОВЫЙ
  └── bin_ingest.rs       → bin/ingest.rs ✓ НОВЫЙ

  В корень проекта:
  └── Cargo.toml          → Cargo.toml (ОБНОВИ)

Команды копирования (на Linux/macOS):

  # Копируем src/ файлы
  cp /home/claude/lib.rs src/
  cp /home/claude/service.rs src/
  cp /home/claude/parse.rs src/
  cp /home/claude/database.rs src/
  cp /home/claude/handler.rs src/

  # Копируем bin/ файлы
  cp /home/claude/bin_server.rs bin/server.rs
  cp /home/claude/bin_ingest.rs bin/ingest.rs

  # Обновляем Cargo.toml
  cp /home/claude/Cargo.toml .

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

⚙️ ШАГ 3: ОБНОВЛЕНИЕ CARGO.toml
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

Если у тебя уже есть Cargo.toml — ВАЖНО:

1. Убедись, что [package] название = "transcriber"
   (или измени на своё имя везде)

2. Добавь нужные зависимости:

   [dependencies]
   axum = { version = "0.7", features = ["ws"] }
   sqlx = { version = "0.7", features = ["runtime-tokio-rustls", "sqlite"] }
   serde = { version = "1.0", features = ["derive"] }
   serde_json = "1.0"
   anyhow = "1.0"
   tokio = { version = "1", features = ["full"] }
   clap = { version = "4", features = ["derive"] }
   regex = "1.10"
   futures-util = "0.3"
   tracing = "0.1"
   tracing-subscriber = "0.3"
   rustls = { version = "0.22", features = ["ring"] }
   rustls-native-certs = "0.7"
   tokio-tungstenite = "0.21"
   axum-server = { version = "0.5", features = ["tls-rustls"] }
   chrono = { version = "0.4", features = ["serde"] }

3. Оставь [lib] и [[bin]] блоки как в скопированном Cargo.toml

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

🧪 ШАГ 4: ПРОВЕРКА СБОРКИ
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

Проверяем, что всё скомпилировалось:

  cargo build 2>&1 | head -50

Если ошибки — скорее всего недостаток зависимостей или опечатка в путях.

Если видишь:
  error[E0583]: file not found for module `service`

Значит файл не скопирован или в неправильное место.

Если ошибки вроде:
  error[E0432]: unresolved import `transcriber::xxx`

Проверь lib.rs — правильно ли экспортированы модули.

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

🔧 ШАГ 5: МАЙНОР ФИКСЫ (если надо)
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

Если в bin/server.rs ошибка про Repository::new() — добавь реализацию:

  В src/repository.rs добавь:

    impl Repository {
        pub fn new(pool: SqlitePool) -> Self {
            Self { pool }
        }
    }

Если ошибка про clone() в Repository — добавь derive:

  #[derive(Clone)]
  pub struct Repository {
      pool: SqlitePool,
  }

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

✅ ШАГ 6: ПРОВЕРКА РАБОТЫ
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

1. Запускаем основной сервер:
   
   cargo run --bin server
   
   Должно вывести:
     ✓ БД готова
     Сервер: https://0.0.0.0:3005

2. Проверяем API здоровья в другом терминале:

   curl http://localhost:3005/health
   
   Ответ: 200 OK

3. Запускаем ingest:

   # Первый — создаём test-файлы
   echo "[00:00] Test text" > recordings/rec_1234567890_seg0001.txt
   touch recordings/rec_1234567890_seg0001.webm
   
   # Загружаем в БД
   cargo run --bin ingest -- --db ./transcripts.db --input ./recordings
   
   Должно вывести:
     ✓ rec_1234567890_seg0001.webm [1 реплик]

4. Проверяем БД:

   sqlite3 transcripts.db "SELECT slug, COUNT(*) FROM sessions GROUP BY slug;"
   
   Должно вывести одну строку с нашей сессией.

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

🚀 ШАГ 7: ДАЛЬШЕ
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

Когда инфраструктура (Фаза 1) работает:

СЛЕДУЮЩИЙ ШАГ → Фаза 2: HTML страница для просмотра

  • Создать pages/transcripts.html
  • Fetch API endpoints (/api/sessions, /api/segments, etc.)
  • Редактирование переводов inline
  • Auto-save PATCH /api/utterances/:id/translation

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

🎯 КРАТКИЙ ЧЕКЛИСТ
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

[ ] Скопировал все файлы в правильные места
[ ] Обновил Cargo.toml (зависимости + [[bin]])
[ ] cargo build завершилась без ошибок
[ ] cargo run --bin server — сервер запустился
[ ] cargo run --bin ingest — ingest загрузил test-файл
[ ] sqlite3 transcripts.db — данные в БД
[ ] Готов к Фазе 2 (HTML UI)

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

❓ ЧАСТО ЗАДАВАЕМЫЕ ВОПРОСЫ
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

Q: Какой именно формат .txt ожидает parse.rs?
A: [MM:SS] текст реплики, по одной строке на реплику.
   Совпадает с выводом transcribe_thai_mac.py (твой Python скрипт).

Q: Что если файла .webm нет, только .txt?
A: ingest.rs пропускает такие пары. Нужно оба файла.

Q: Можно ли перепроцессировать старые файлы?
A: Нет — ingest проверяет БД. Если нужно перезагрузить:
   DELETE FROM segments WHERE filename = '...';
   cargo run --bin ingest

Q: Как добавить свои speaker'ы в UI?
A: Расширить handler.rs с POST /api/speakers/:session_id
   Потом на странице добавить интерфейс для их создания.

Q: Как интегрировать диаризацию (pyannote)?
A: Запустить pyannote отдельно → обновить speaker_id в utterances
   Модель уже готова, нужен только код обновления.

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━