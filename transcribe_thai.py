#!/usr/bin/env python3
"""
Транскрибация тайских аудио файлов (.webm) через faster-whisper.
Модель: biodatlab/whisper-th-large-v3

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

def load_model():
    from faster_whisper import WhisperModel
    print("Загрузка модели biodatlab/whisper-th-large-v3 ...")
    model = WhisperModel(
        "Vinxscribe/biodatlab-whisper-th-large-v3-faster",
        device="cuda" if _has_cuda() else "cpu",
        compute_type="float16" if _has_cuda() else "int8",
    )
    print("Модель загружена.\n")
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

# ─── Транскрибация одного файла ───────────────────────────────────────────────

def transcribe_file(model, webm_path: Path) -> Path | None:
    """
    Транскрибирует webm_path, сохраняет .txt рядом.
    Возвращает путь к txt или None при ошибке.
    """
    txt_path = webm_path.with_suffix(".txt")

    if txt_path.exists():
        print(f"  [пропуск] {webm_path.name} — txt уже существует")
        return txt_path

    print(f"  → {webm_path.name}", end=" ", flush=True)
    t0 = time.time()

    try:
        segments, info = model.transcribe(
            str(webm_path),
            language="th",           # тайский
            beam_size=5,
            vad_filter=True,         # убирает тишину
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
                # Временна́я метка [MM:SS] в начале каждого сегмента
                mm = int(seg.start) // 60
                ss = int(seg.start) % 60
                lines.append(f"[{mm:02d}:{ss:02d}] {text}")

        elapsed = time.time() - t0
        duration = info.duration or 0

        if lines:
            txt_path.write_text("\n".join(lines), encoding="utf-8")
            print(f"✓  {len(lines)} сегм. | {duration:.0f}с аудио | {elapsed:.1f}с")
        else:
            # Пустой файл — тишина или нераспознанная речь
            txt_path.write_text("", encoding="utf-8")
            print(f"~  тишина / нет речи | {elapsed:.1f}с")

        return txt_path

    except Exception as e:
        print(f"✗  ошибка: {e}")
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
    """
    Следит за папкой. Когда появляется новый .webm без .txt — транскрибирует.
    Файл считается готовым если его размер не менялся poll_interval секунд
    (защита от частичной записи).
    """
    print(f"Слежение за {folder}  (Ctrl+C для выхода)\n")
    seen: dict[Path, int] = {}  # path → size при последней проверке

    try:
        while True:
            for f in sorted(folder.glob("*.webm")):
                txt = f.with_suffix(".txt")
                if txt.exists():
                    continue

                size = f.stat().st_size
                prev = seen.get(f)

                if prev is None:
                    seen[f] = size          # первый раз увидели
                elif size == prev:
                    # Размер не изменился — файл дописан, можно читать
                    transcribe_file(model, f)
                    seen.pop(f, None)
                else:
                    seen[f] = size          # ещё пишется

            time.sleep(poll_interval)

    except KeyboardInterrupt:
        print("\nОстановлено.")

# ─── CLI ──────────────────────────────────────────────────────────────────────

def main():
    parser = argparse.ArgumentParser(description="Транскрибация тайских .webm через faster-whisper")
    parser.add_argument(
        "target",
        help="Папка с .webm файлами или один .webm файл",
    )
    parser.add_argument(
        "--watch", "-w",
        action="store_true",
        help="Следить за папкой и транскрибировать новые файлы по мере появления",
    )
    parser.add_argument(
        "--reprocess",
        action="store_true",
        help="Перезаписать уже существующие .txt файлы",
    )
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
            # В режиме watch сначала обрабатываем накопленное, потом следим
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
