#!/usr/bin/env python3
"""
Транскрибация тайских аудио файлов (.webm / .wav) через mlx-whisper.
Оптимизировано для Apple Silicon (M1/M2/M3/M4) — инференс на GPU через MLX.

Модель: mlx-community/whisper-large-v3-mlx

Использование:
    python transcribe_thai_mac.py recordings/          # вся папка
    python transcribe_thai_mac.py recordings/ --watch  # следить за новыми файлами
    python transcribe_thai_mac.py rec_XXX_seg0001.webm # один файл
    python transcribe_thai_mac.py cli_XXX_seg0001.wav  # один wav файл
"""

import sys
import time
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


# ─── Транскрибация одного файла ───────────────────────────────────────────────

def transcribe_file(audio_path: Path, model_id: str) -> Path | None:
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
        lines = []
        for seg in segments:
            text = seg.get("text", "").strip()
            if text:
                start = seg.get("start", 0.0)
                mm = int(start) // 60
                ss = int(start) % 60
                lines.append(f"[{mm:02d}:{ss:02d}] {text}")

        elapsed = time.time() - t0

        # Длительность — берём end последнего сегмента если есть
        duration = segments[-1].get("end", 0.0) if segments else 0.0

        if lines:
            txt_path.write_text("\n".join(lines), encoding="utf-8")
            print(f"✓  {len(lines)} сегм. | {duration:.0f}с аудио | {elapsed:.1f}с")
        else:
            txt_path.write_text("", encoding="utf-8")
            print(f"~  тишина / нет речи | {elapsed:.1f}с")

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

def process_directory(folder: Path, model_id: str, skip_existing: bool = True):
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
        result = transcribe_file(f, model_id)
        if result is not None:
            ok += 1
        else:
            err += 1

    print(f"\nГотово: {ok} обработано, {skip} пропущено, {err} ошибок")


# ─── Режим слежения (--watch) ─────────────────────────────────────────────────

def watch_directory(folder: Path, model_id: str, poll_interval: float = 5.0):
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
                    transcribe_file(f, model_id)
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
    args = parser.parse_args()

    model_id = args.model

    # Выводим информацию о среде запуска
    chip = get_device_label()
    print(f"\n  Инференс : Apple GPU (MLX)")
    print(f"  Чип      : {chip}")
    print(f"  Модель   : {model_id}")
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
        transcribe_file(target, model_id)

    elif target.is_dir():
        if args.watch:
            process_directory(target, model_id, skip_existing=not args.reprocess)
            print()
            watch_directory(target, model_id)
        else:
            process_directory(target, model_id, skip_existing=not args.reprocess)
    else:
        print(f"Ошибка: {target} — не файл и не папка")
        sys.exit(1)


if __name__ == "__main__":
    main()