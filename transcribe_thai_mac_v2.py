#!/usr/bin/env python3
"""
Транскрибация тайских аудио файлов (.webm / .wav) через mlx-whisper.
Оптимизировано для Apple Silicon (M1/M2/M3/M4) — инференс на GPU через MLX.

Модель: mlx-community/whisper-large-v3-mlx

Результат транскрибации:
  1. ВСЕГДА сохраняется в .txt рядом с аудио файлом (как раньше) — это
     гарантированный fallback, не зависящий от базы данных.
  2. ДОПОЛНИТЕЛЬНО (best-effort) пишется в SQLite-базу (ту же, что использует
     Rust-сервер: sessions / segments / utterances), чтобы реплики сразу были
     видны в /sessions и /batch без отдельного шага импорта.
     Если запись в БД не удалась по любой причине (БД занята, нет файла,
     битая схема и т.п.) — ошибка не фатальна: .txt уже на диске, скрипт
     просто выводит предупреждение и продолжает работу со следующим файлом.

Использование:
    python transcribe_thai_mac.py recordings/                 # вся папка
    python transcribe_thai_mac.py recordings/ --watch          # следить за новыми файлами
    python transcribe_thai_mac.py rec_XXX_seg0001.webm         # один файл
    python transcribe_thai_mac.py cli_XXX_seg0001.wav          # один wav файл
    python transcribe_thai_mac.py recordings/ --db custom.db   # другой путь к БД
    python transcribe_thai_mac.py recordings/ --no-db          # вообще без БД, только .txt
"""

import sys
import time
import sqlite3
import platform
import argparse
from pathlib import Path

# ─── Конфигурация ─────────────────────────────────────────────────────────────

# MLX-модели живут в отдельных репо на HuggingFace с суффиксом -mlx.
# large-v3 — лучшее качество, ~3 GB, отлично работает на M1+ с 16 GB RAM.
# Альтернативы (меньше RAM, быстрее):
#   mlx-community/whisper-large-v3-turbo          (~1.6 GB, чуть хуже качество)
#   mlx-community/whisper-medium-mlx              (~1.5 GB)
#   mlx-community/whisper-small-mlx               (~500 MB)
# Лучшая доступная MLX-модель для тайского языка.
# whisper-large-v3 обучена на тайском — качество сопоставимо с biodatlab fine-tune.
# Альтернативы (быстрее, чуть хуже):
#   mlx-community/whisper-large-v3-turbo   (~1.6 GB)
#   mlx-community/whisper-medium-mlx       (~1.5 GB)
MODEL_ID = "mlx-community/whisper-large-v3-mlx"

SUPPORTED_EXTENSIONS = {".webm", ".wav"}

# Путь к БД по умолчанию — совпадает с DB_URL в main.rs ("sqlite://transcripts.db"),
# то есть файл transcripts.db в текущей рабочей директории.
DEFAULT_DB_PATH = "transcripts.db"


# ─── Определение устройства ───────────────────────────────────────────────────

def get_device_label() -> str:
    """Возвращает строку с описанием чипа для вывода в терминал."""
    if platform.system() != "Darwin":
        return platform.processor() or platform.machine() or "Unknown"

    # Пробуем получить название чипа через system_profiler
    import subprocess
    try:
        out = subprocess.check_output(
            ["sysctl", "-n", "machdep.cpu.brand_string"],
            stderr=subprocess.DEVNULL,
            timeout=3,
        ).decode().strip()
        if out:
            return out
    except Exception:
        pass

    return platform.processor() or "Apple Silicon"


# ─── Фикс WebM duration ──────────────────────────────────────────────────────

def fix_webm_duration(webm_path: Path) -> Path:
    """
    MediaRecorder не пишет duration в заголовок WebM (всегда Infinity).
    Фикс: remux через ffmpeg без перекодирования.
    Если ffmpeg не установлен — возвращаем оригинал.
    """
    import subprocess
    import shutil

    if not shutil.which("ffmpeg"):
        return webm_path

    fixed_path = webm_path.with_name(webm_path.stem + "_fixed.webm")
    if fixed_path.exists():
        return fixed_path

    try:
        result = subprocess.run(
            [
                "ffmpeg", "-y",
                "-i", str(webm_path),
                "-c", "copy",
                "-fflags", "+genpts",
                str(fixed_path),
            ],
            capture_output=True,
            timeout=60,
        )
        if result.returncode == 0 and fixed_path.exists():
            return fixed_path
        if fixed_path.exists():
            fixed_path.unlink()
        return webm_path
    except Exception:
        return webm_path


def prepare_input(audio_path: Path) -> tuple[Path, bool]:
    """
    Возвращает (путь_к_файлу, нужно_ли_удалить_после).
    .wav  — читается напрямую.
    .webm — прогоняется через ffmpeg-фикс.
    """
    ext = audio_path.suffix.lower()
    if ext == ".webm":
        fixed = fix_webm_duration(audio_path)
        return fixed, (fixed != audio_path)
    return audio_path, False


# ─── SQLite: схема и доступ к БД ──────────────────────────────────────────────
#
# Схема СОЗНАТЕЛЬНО продублирована из src/repository.rs (Repository::migrate),
# вплоть до названий колонок (idx, а не index — зарезервированное слово) и
# constraint'ов. Это позволяет писать в ту же базу, что использует Rust-сервер,
# без необходимости держать общий код. CREATE TABLE IF NOT EXISTS — безопасно
# запускать даже если сервер уже создал схему сам.

DB_SCHEMA_SQL = """
CREATE TABLE IF NOT EXISTS sessions (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    slug        TEXT    NOT NULL UNIQUE,
    created_at  INTEGER NOT NULL,
    note        TEXT
);

CREATE TABLE IF NOT EXISTS segments (
    id                    INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id            INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    idx                   INTEGER NOT NULL,
    filename              TEXT    NOT NULL UNIQUE,
    size_bytes            INTEGER,
    recorded_at           INTEGER NOT NULL,
    transcription_status  TEXT    NOT NULL DEFAULT 'pending'
        CHECK(transcription_status IN ('pending','processing','done','error')),
    error_message         TEXT,
    UNIQUE(session_id, idx)
);
CREATE INDEX IF NOT EXISTS idx_segments_session ON segments(session_id);

CREATE TABLE IF NOT EXISTS speakers (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id  INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    label       TEXT    NOT NULL,
    color       TEXT
);
CREATE INDEX IF NOT EXISTS idx_speakers_session ON speakers(session_id);

CREATE TABLE IF NOT EXISTS utterances (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    segment_id  INTEGER NOT NULL REFERENCES segments(id) ON DELETE CASCADE,
    idx         INTEGER NOT NULL,
    start_sec   REAL    NOT NULL,
    end_sec     REAL    NOT NULL,
    text_thai   TEXT    NOT NULL,
    text_ru     TEXT,
    speaker_id  INTEGER REFERENCES speakers(id) ON DELETE SET NULL,
    confidence  REAL,
    UNIQUE(segment_id, idx)
);
CREATE INDEX IF NOT EXISTS idx_utterances_segment ON utterances(segment_id);
CREATE INDEX IF NOT EXISTS idx_utterances_speaker ON utterances(speaker_id);
"""


def connect_db(db_path: str) -> sqlite3.Connection:
    """
    Открывает (или создаёт) SQLite-базу и гарантирует наличие схемы.
    busy_timeout — на случай, если Rust-сервер в этот момент держит запись
    (WAL допускает параллельные читатели + одного писателя; ждём, а не падаем).
    """
    conn = sqlite3.connect(db_path, timeout=10)
    conn.execute("PRAGMA journal_mode=WAL")
    conn.execute("PRAGMA synchronous=NORMAL")
    conn.execute("PRAGMA foreign_keys=ON")
    conn.execute("PRAGMA busy_timeout=10000")
    conn.executescript(DB_SCHEMA_SQL)
    return conn


def parse_audio_filename(filename: str) -> tuple[str, int]:
    """
    Парсит «rec_1234567890_seg0001.webm» / «cli_1234567890_seg0001.wav»
    → (session_slug, segment_index). Логика зеркальна Rust-функции
    parse_audio_filename из src/service.rs.
    """
    stem = Path(filename).stem
    parts = stem.split("_seg")
    if len(parts) != 2:
        raise ValueError(
            f"Имя файла должно быть вида {{rec|cli}}_TIMESTAMP_segNNNN: {filename}"
        )
    slug, idx_str = parts
    try:
        index = int(idx_str)
    except ValueError as e:
        raise ValueError(f"Индекс сегмента должен быть числом: {filename}") from e
    return slug, index


def get_or_create_session(conn: sqlite3.Connection, slug: str) -> int:
    now = int(time.time())
    conn.execute(
        "INSERT OR IGNORE INTO sessions (slug, created_at, note) VALUES (?, ?, NULL)",
        (slug, now),
    )
    row = conn.execute("SELECT id FROM sessions WHERE slug = ?", (slug,)).fetchone()
    if row is None:
        raise RuntimeError(f"Не удалось получить/создать сессию '{slug}'")
    return row[0]


def upsert_segment(
    conn: sqlite3.Connection,
    session_id: int,
    index: int,
    filename: str,
    size_bytes: int | None,
    recorded_at: int,
    status: str = "done",
    error_message: str | None = None,
) -> int:
    """Создаёт сегмент, либо обновляет уже существующий (идемпотентно по filename)."""
    existing = conn.execute(
        "SELECT id FROM segments WHERE filename = ?", (filename,)
    ).fetchone()

    if existing:
        seg_id = existing[0]
        conn.execute(
            """UPDATE segments
               SET session_id = ?, idx = ?, size_bytes = ?, recorded_at = ?,
                   transcription_status = ?, error_message = ?
               WHERE id = ?""",
            (session_id, index, size_bytes, recorded_at, status, error_message, seg_id),
        )
        return seg_id

    cur = conn.execute(
        """INSERT INTO segments
           (session_id, idx, filename, size_bytes, recorded_at, transcription_status, error_message)
           VALUES (?, ?, ?, ?, ?, ?, ?)""",
        (session_id, index, filename, size_bytes, recorded_at, status, error_message),
    )
    return cur.lastrowid


def replace_utterances(
    conn: sqlite3.Connection, segment_id: int, utterances: list[dict]
) -> None:
    """Удаляет старые реплики сегмента и вставляет новые — как replace_utterances_for_segment в Rust."""
    conn.execute("DELETE FROM utterances WHERE segment_id = ?", (segment_id,))
    conn.executemany(
        """INSERT INTO utterances
           (segment_id, idx, start_sec, end_sec, text_thai, text_ru, speaker_id, confidence)
           VALUES (?, ?, ?, ?, ?, ?, ?, ?)""",
        [
            (segment_id, i, u["start"], u["end"], u["text"], None, None, None)
            for i, u in enumerate(utterances)
        ],
    )


def save_transcription_to_db(
    audio_path: Path, utterances: list[dict], db_path: str
) -> int:
    """
    Best-effort запись результата транскрибации в SQLite.
    Бросает исключение наверх при любой проблеме — вызывающий код решает,
    что с этим делать (у нас — просто предупреждение, .txt уже сохранён).
    Возвращает количество записанных реплик.
    """
    filename = audio_path.name
    slug, index = parse_audio_filename(filename)

    stat = audio_path.stat()
    size_bytes = stat.st_size
    recorded_at = int(stat.st_mtime)

    conn = connect_db(db_path)
    try:
        with conn:  # транзакция: либо всё, либо ничего
            session_id = get_or_create_session(conn, slug)
            segment_id = upsert_segment(
                conn,
                session_id=session_id,
                index=index,
                filename=filename,
                size_bytes=size_bytes,
                recorded_at=recorded_at,
                status="done",
            )
            replace_utterances(conn, segment_id, utterances)
        return len(utterances)
    finally:
        conn.close()


# ─── Транскрибация одного файла ───────────────────────────────────────────────

def transcribe_file(audio_path: Path, model_id: str, db_path: str | None) -> Path | None:
    import mlx_whisper

    txt_path = audio_path.with_suffix(".txt")

    if txt_path.exists():
        print(f"  [пропуск] {audio_path.name} — txt уже существует")
        return txt_path

    print(f"  → {audio_path.name}", end=" ", flush=True)
    t0 = time.time()

    input_path, should_delete = prepare_input(audio_path)

    try:
        result = mlx_whisper.transcribe(
            str(input_path),
            path_or_hf_repo=model_id,
            language="th",
            # beam_size не поддерживается в mlx_whisper напрямую —
            # используется greedy decoding (быстро и качественно на large-v3)
            word_timestamps=False,
            verbose=False,
        )

        segments = result.get("segments", [])

        # Строки для .txt (человекочитаемый формат с таймкодами)
        lines = []
        # Те же данные, но «сырые» (нужны для записи в БД: start_sec/end_sec как float)
        utterances_for_db = []

        for seg in segments:
            text = seg.get("text", "").strip()
            if not text:
                continue
            start = seg.get("start", 0.0)
            end = seg.get("end", start)
            mm = int(start) // 60
            ss = int(start) % 60
            lines.append(f"[{mm:02d}:{ss:02d}] {text}")
            utterances_for_db.append({"start": float(start), "end": float(end), "text": text})

        elapsed = time.time() - t0

        # Длительность — берём end последнего сегмента если есть
        duration = segments[-1].get("end", 0.0) if segments else 0.0

        # ── 1. .txt — пишем ВСЕГДА первым, независимо от БД ──────────────────
        if lines:
            txt_path.write_text("\n".join(lines), encoding="utf-8")
            print(f"✓  {len(lines)} сегм. | {duration:.0f}с аудио | {elapsed:.1f}с")
        else:
            txt_path.write_text("", encoding="utf-8")
            print(f"~  тишина / нет речи | {elapsed:.1f}с")

        # ── 2. БД — best-effort, не должна ронять обработку файла ────────────
        if db_path is not None:
            try:
                n = save_transcription_to_db(audio_path, utterances_for_db, db_path)
                print(f"    ↳ записано в БД: {n} реплик")
            except Exception as e:
                print(f"    ↳ ⚠ не удалось записать в БД ({e}) — оставлен только .txt")

        if should_delete and input_path.exists():
            input_path.unlink()

        return txt_path

    except Exception as e:
        print(f"✗  ошибка: {e}")
        if should_delete and input_path.exists():
            input_path.unlink()
        return None


# ─── Сбор файлов из папки ────────────────────────────────────────────────────

def collect_audio_files(folder: Path) -> list[Path]:
    files = []
    for ext in SUPPORTED_EXTENSIONS:
        files.extend(folder.glob(f"*{ext}"))
    return sorted(set(files))


# ─── Пакетная обработка папки ─────────────────────────────────────────────────

def process_directory(
    folder: Path, model_id: str, db_path: str | None, skip_existing: bool = True
):
    files = collect_audio_files(folder)
    if not files:
        print(f"Нет аудио файлов ({', '.join(SUPPORTED_EXTENSIONS)}) в {folder}")
        return

    webm_count = sum(1 for f in files if f.suffix == ".webm")
    wav_count  = sum(1 for f in files if f.suffix == ".wav")
    print(f"Найдено файлов: {len(files)}  (.webm: {webm_count}, .wav: {wav_count})\n")

    ok = err = skip = 0

    for f in files:
        if skip_existing and f.with_suffix(".txt").exists():
            skip += 1
            continue
        result = transcribe_file(f, model_id, db_path)
        if result is not None:
            ok += 1
        else:
            err += 1

    print(f"\nГотово: {ok} обработано, {skip} пропущено, {err} ошибок")


# ─── Режим слежения (--watch) ─────────────────────────────────────────────────

def watch_directory(folder: Path, model_id: str, db_path: str | None, poll_interval: float = 5.0):
    print(f"Слежение за {folder}  (Ctrl+C для выхода)\n")
    seen: dict[Path, int] = {}

    try:
        while True:
            for f in collect_audio_files(folder):
                if f.with_suffix(".txt").exists():
                    continue

                size = f.stat().st_size
                prev = seen.get(f)

                if prev is None:
                    seen[f] = size
                elif size == prev:
                    transcribe_file(f, model_id, db_path)
                    seen.pop(f, None)
                else:
                    seen[f] = size

            time.sleep(poll_interval)

    except KeyboardInterrupt:
        print("\nОстановлено.")


# ─── Проверка платформы ───────────────────────────────────────────────────────

def check_platform():
    if platform.system() != "Darwin":
        print("⚠  Этот скрипт оптимизирован для macOS (Apple Silicon).")
        print("   На других платформах используйте transcribe_thai.py (faster-whisper).\n")

    if platform.machine() != "arm64":
        print("⚠  Обнаружен Intel Mac — MLX работает только на Apple Silicon (M1+).")
        print("   Скрипт завершится с ошибкой при импорте mlx_whisper.\n")
        sys.exit(1)


# ─── CLI ──────────────────────────────────────────────────────────────────────

def main():
    check_platform()

    parser = argparse.ArgumentParser(
        description="Транскрибация тайских .webm/.wav через mlx-whisper (Apple Silicon GPU)"
    )
    parser.add_argument(
        "target",
        help="Папка с аудио файлами или один .webm/.wav файл"
    )
    parser.add_argument(
        "--watch", "-w",
        action="store_true",
        help="Следить за папкой и транскрибировать новые файлы по мере появления"
    )
    parser.add_argument(
        "--reprocess",
        action="store_true",
        help="Перезаписать уже существующие .txt файлы"
    )
    parser.add_argument(
        "--model",
        default=MODEL_ID,
        help=f"HuggingFace repo MLX-модели (по умолчанию: {MODEL_ID})"
    )
    parser.add_argument(
        "--db",
        default=DEFAULT_DB_PATH,
        help=f"Путь к SQLite-базе для записи результатов (по умолчанию: {DEFAULT_DB_PATH})"
    )
    parser.add_argument(
        "--no-db",
        action="store_true",
        help="Не писать в БД вообще, сохранять только .txt"
    )
    args = parser.parse_args()

    model_id = args.model
    db_path = None if args.no_db else args.db

    # Выводим информацию о среде запуска
    chip = get_device_label()
    print(f"\n  Инференс : Apple GPU (MLX)")
    print(f"  Чип      : {chip}")
    print(f"  Модель   : {model_id}")
    print(f"  БД       : {db_path if db_path else 'отключена (--no-db), только .txt'}")
    print()

    # Прогреваем импорт mlx_whisper здесь чтобы поймать ошибку до обработки файлов
    try:
        import mlx_whisper  # noqa: F401
    except ImportError:
        print("✗  mlx-whisper не установлен. Установите: uv add mlx-whisper")
        sys.exit(1)

    target = Path(args.target)

    if not target.exists():
        print(f"Ошибка: {target} не существует")
        sys.exit(1)

    if target.is_file():
        if target.suffix.lower() not in SUPPORTED_EXTENSIONS:
            print(f"Ошибка: файл должен быть одним из {SUPPORTED_EXTENSIONS}")
            sys.exit(1)
        transcribe_file(target, model_id, db_path)

    elif target.is_dir():
        if args.watch:
            process_directory(target, model_id, db_path, skip_existing=not args.reprocess)
            print()
            watch_directory(target, model_id, db_path)
        else:
            process_directory(target, model_id, db_path, skip_existing=not args.reprocess)
    else:
        print(f"Ошибка: {target} — не файл и не папка")
        sys.exit(1)


if __name__ == "__main__":
    main()