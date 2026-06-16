#!/usr/bin/env python3
"""
Транскрибация тайских аудио файлов (.webm / .wav) через faster-whisper.
Модель: Vinxscribe/biodatlab-whisper-th-large-v3-faster

Использование:
    python transcribe_thai.py recordings/          # вся папка
    python transcribe_thai.py recordings/ --watch  # следить за новыми файлами
    python transcribe_thai.py rec_XXX_seg0001.webm # один файл
    python transcribe_thai.py cli_XXX_seg0001.wav  # один wav файл
"""

import sys
import time
import argparse
from pathlib import Path

# ─── Загрузка модели ──────────────────────────────────────────────────────────

MODEL_ID = "Vinxscribe/biodatlab-whisper-th-large-v3-faster"
CACHE_DIR = Path.home() / ".cache" / "huggingface" / "hub"

SUPPORTED_EXTENSIONS = {".webm", ".wav"}


def _model_cached() -> bool:
    """Проверяет есть ли модель в локальном кэше."""
    slug = "models--" + MODEL_ID.replace("/", "--")
    model_bin = CACHE_DIR / slug
    return model_bin.exists() and any(model_bin.rglob("model.bin"))


def load_model():
    from faster_whisper import WhisperModel

    cached = _model_cached()
    if cached:
        print(f"Модель найдена в кэше, загружаем...")
    else:
        print(f"Скачиваем модель {MODEL_ID} (~3 GB), это займёт несколько минут...")
        print("Прогресс скачивания показывается ниже:\n")

    model = WhisperModel(
        MODEL_ID,
        device="cuda" if _has_cuda() else "cpu",
        compute_type="float16" if _has_cuda() else "int8",
    )
    print("\nМодель готова.\n")
    return model


def _has_cuda():
    try:
        import torch
        return torch.cuda.is_available()
    except ImportError:
        pass
    try:
        import ctypes
        ctypes.CDLL("libcuda.so")
        return True
    except Exception:
        return False


# ─── Фикс WebM duration ──────────────────────────────────────────────────────

def fix_webm_duration(webm_path: Path) -> Path:
    """
    MediaRecorder не пишет duration в заголовок WebM (всегда Infinity).
    Из-за этого Whisper получает файл без временной шкалы → все сегменты в одной точке.

    Фикс: перекодируем через ffmpeg с remux (копируем потоки без перекодирования).
    Если ffmpeg не установлен — возвращаем оригинал.
    """
    import subprocess, shutil

    if not shutil.which("ffmpeg"):
        return webm_path  # ffmpeg не найден — работаем как раньше

    fixed_path = webm_path.with_name(webm_path.stem + "_fixed.webm")
    if fixed_path.exists():
        return fixed_path

    try:
        result = subprocess.run(
            [
                "ffmpeg", "-y",
                "-i", str(webm_path),
                "-c", "copy",          # без перекодирования — просто remux
                "-fflags", "+genpts",  # генерировать PTS если отсутствуют
                str(fixed_path),
            ],
            capture_output=True,
            timeout=60,
        )
        if result.returncode == 0 and fixed_path.exists():
            return fixed_path
        else:
            if fixed_path.exists():
                fixed_path.unlink()
            return webm_path
    except Exception:
        return webm_path


def prepare_input(audio_path: Path) -> tuple[Path, bool]:
    """
    Подготавливает файл к транскрибации.
    Возвращает (путь_к_файлу, нужно_ли_удалить_после).
    - .wav — читается напрямую, сервер пишет корректный заголовок
    - .webm — прогоняется через fix_webm_duration
    """
    ext = audio_path.suffix.lower()
    if ext == ".wav":
        return audio_path, False
    elif ext == ".webm":
        fixed = fix_webm_duration(audio_path)
        return fixed, (fixed != audio_path)
    else:
        return audio_path, False


# ─── Транскрибация одного файла ───────────────────────────────────────────────

def transcribe_file(model, audio_path: Path) -> Path | None:
    txt_path = audio_path.with_suffix(".txt")

    if txt_path.exists():
        print(f"  [пропуск] {audio_path.name} — txt уже существует")
        return txt_path

    print(f"  → {audio_path.name}", end=" ", flush=True)
    t0 = time.time()

    input_path, should_delete = prepare_input(audio_path)

    try:
        segments, info = model.transcribe(
            str(input_path),
            language="th",
            beam_size=5,
            vad_filter=True,
            vad_parameters={
                "min_silence_duration_ms": 500,
                "speech_pad_ms": 200,
            },
            word_timestamps=False,
        )

        lines = []
        for seg in segments:
            text = seg.text.strip()
            if text:
                mm = int(seg.start) // 60
                ss = int(seg.start) % 60
                lines.append(f"[{mm:02d}:{ss:02d}] {text}")

        elapsed = time.time() - t0
        duration = info.duration or 0

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
    """Собирает .webm и .wav файлы, сортирует по имени."""
    files = []
    for ext in SUPPORTED_EXTENSIONS:
        files.extend(folder.glob(f"*{ext}"))
    return sorted(set(files))


# ─── Пакетная обработка папки ─────────────────────────────────────────────────

def process_directory(model, folder: Path, skip_existing: bool = True):
    files = collect_audio_files(folder)
    if not files:
        print(f"Нет аудио файлов ({', '.join(SUPPORTED_EXTENSIONS)}) в {folder}")
        return

    webm_count = sum(1 for f in files if f.suffix == ".webm")
    wav_count  = sum(1 for f in files if f.suffix == ".wav")
    print(f"Найдено файлов: {len(files)}  (.webm: {webm_count}, .wav: {wav_count})\n")

    ok = err = skip = 0

    for f in files:
        txt = f.with_suffix(".txt")
        if skip_existing and txt.exists():
            skip += 1
            continue
        result = transcribe_file(model, f)
        if result is not None:
            ok += 1
        else:
            err += 1

    print(f"\nГотово: {ok} обработано, {skip} пропущено, {err} ошибок")


# ─── Режим слежения (--watch) ─────────────────────────────────────────────────

def watch_directory(model, folder: Path, poll_interval: float = 5.0):
    print(f"Слежение за {folder}  (Ctrl+C для выхода)\n")
    seen: dict[Path, int] = {}

    try:
        while True:
            for f in collect_audio_files(folder):
                txt = f.with_suffix(".txt")
                if txt.exists():
                    continue

                size = f.stat().st_size
                prev = seen.get(f)

                if prev is None:
                    seen[f] = size
                elif size == prev:
                    transcribe_file(model, f)
                    seen.pop(f, None)
                else:
                    seen[f] = size

            time.sleep(poll_interval)

    except KeyboardInterrupt:
        print("\nОстановлено.")


# ─── CLI ──────────────────────────────────────────────────────────────────────

def main():
    parser = argparse.ArgumentParser(
        description="Транскрибация тайских .webm/.wav через faster-whisper"
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
    args = parser.parse_args()

    target = Path(args.target)

    if not target.exists():
        print(f"Ошибка: {target} не существует")
        sys.exit(1)

    model = load_model()

    if target.is_file():
        if target.suffix.lower() not in SUPPORTED_EXTENSIONS:
            print(f"Ошибка: файл должен быть одним из {SUPPORTED_EXTENSIONS}")
            sys.exit(1)
        transcribe_file(model, target)

    elif target.is_dir():
        if args.watch:
            process_directory(model, target, skip_existing=not args.reprocess)
            print()
            watch_directory(model, target)
        else:
            process_directory(model, target, skip_existing=not args.reprocess)
    else:
        print(f"Ошибка: {target} — не файл и не папка")
        sys.exit(1)


if __name__ == "__main__":
    main()