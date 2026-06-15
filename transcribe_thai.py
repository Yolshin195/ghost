#!/usr/bin/env python3
"""
Транскрибация тайских аудио файлов (.webm) через faster-whisper.
Модель: Vinxscribe/biodatlab-whisper-th-large-v3-faster

Использование:
    python transcribe_thai.py recordings/          # вся папка
    python transcribe_thai.py recordings/ --watch  # следить за новыми файлами
    python transcribe_thai.py rec_XXX_seg0001.webm # один файл
"""

import sys
import time
import argparse
from pathlib import Path

# ─── Загрузка модели ──────────────────────────────────────────────────────────

MODEL_ID = "Vinxscribe/biodatlab-whisper-th-large-v3-faster"
CACHE_DIR = Path.home() / ".cache" / "huggingface" / "hub"

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
    Из-за этого:
      - браузер показывает полосу прокрутки всегда в конце
      - Whisper получает файл без временной шкалы → все сегменты в одной точке

    Фикс: перекодируем через ffmpeg с remux (копируем потоки без перекодирования).
    Если ffmpeg не установлен — возвращаем оригинал.
    """
    import subprocess, shutil, tempfile

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


# ─── Транскрибация одного файла ───────────────────────────────────────────────

def transcribe_file(model, webm_path: Path) -> Path | None:
    txt_path = webm_path.with_suffix(".txt")

    if txt_path.exists():
        print(f"  [пропуск] {webm_path.name} — txt уже существует")
        return txt_path

    print(f"  → {webm_path.name}", end=" ", flush=True)
    t0 = time.time()

    # Фикс duration перед транскрибацией
    input_path = fix_webm_duration(webm_path)

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

        # Удаляем временный fixed файл после транскрибации
        if input_path != webm_path and input_path.exists():
            input_path.unlink()

        return txt_path

    except Exception as e:
        print(f"✗  ошибка: {e}")
        if input_path != webm_path and input_path.exists():
            input_path.unlink()
        return None

# ─── Пакетная обработка папки ─────────────────────────────────────────────────

def process_directory(model, folder: Path, skip_existing: bool = True):
    files = sorted(folder.glob("*.webm"))
    if not files:
        print(f"Нет .webm файлов в {folder}")
        return

    print(f"Найдено файлов: {len(files)}\n")
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
            for f in sorted(folder.glob("*.webm")):
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
    parser = argparse.ArgumentParser(description="Транскрибация тайских .webm через faster-whisper")
    parser.add_argument("target", help="Папка с .webm файлами или один .webm файл")
    parser.add_argument("--watch", "-w", action="store_true",
                        help="Следить за папкой и транскрибировать новые файлы по мере появления")
    parser.add_argument("--reprocess", action="store_true",
                        help="Перезаписать уже существующие .txt файлы")
    args = parser.parse_args()

    target = Path(args.target)

    if not target.exists():
        print(f"Ошибка: {target} не существует")
        sys.exit(1)

    model = load_model()

    if target.is_file():
        if target.suffix.lower() != ".webm":
            print("Ошибка: файл должен быть .webm")
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