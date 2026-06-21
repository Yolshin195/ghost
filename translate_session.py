#!/usr/bin/env python3
"""
translate_session.py — автоматический перевод сессии Ghost с тайского на
русский локальной LLM через MLX (GPU Apple Silicon, M3 и т.п.).

Работает поверх уже существующего HTTP API сервера (того же, что использует
страница /batch для ручного экспорта-импорта):
  GET   /api/sessions/{id}                       — читаем реплики
  PATCH /api/utterances/{id}/translation          — сохраняем переводы

Прямого доступа к SQLite не делаем специально — у Repository
max_connections(1), и параллельная запись из двух процессов
(сервер + скрипт) приведёт к блокировкам. Через HTTP API безопасно.

УСТАНОВКА
─────────
    pip install mlx-lm requests

Модель скачивается автоматически при первом запуске (Hugging Face Hub),
дальше берётся из кеша ~/.cache/huggingface.

Рекомендации по модели в зависимости от объёма unified memory на Mac:

    8 ГБ RAM   →  mlx-community/Qwen2.5-7B-Instruct-4bit    (~4.5 ГБ)
    16 ГБ RAM  →  mlx-community/Qwen2.5-14B-Instruct-4bit   (~9 ГБ)   ← по умолчанию
    24+ ГБ RAM →  mlx-community/Qwen2.5-32B-Instruct-4bit   (~19 ГБ)

Qwen2.5 выбран по умолчанию — у него заметно лучше тайский и русский, чем у
большинства открытых моделей такого размера. Можно подставить любую другую
mlx-community-модель через --model.

ИСПОЛЬЗОВАНИЕ
─────────────
    # Список сессий и сколько в них непереведённых реплик
    python translate_session.py --list

    # Перевести сессию целиком (сохраняет в БД через API по ходу работы)
    python translate_session.py --session-id 3

    # Другая модель / другой размер чанка
    python translate_session.py --session-id 3 \
        --model mlx-community/Qwen2.5-7B-Instruct-4bit --chunk-size 15

    # Прогон без сохранения в БД — посмотреть, что получится
    python translate_session.py --session-id 3 --dry-run
"""

import argparse
import json
import re
import sys
import time

import requests
import urllib3

urllib3.disable_warnings(urllib3.exceptions.InsecureRequestWarning)

DEFAULT_BASE_URL = "https://localhost:3005"
DEFAULT_MODEL = "mlx-community/Qwen2.5-14B-Instruct-4bit"
DEFAULT_CHUNK_SIZE = 25
CONTEXT_WINDOW = 8  # сколько последних переведённых реплик давать модели как контекст


def fmt_time(sec: float) -> str:
    s = int(sec)
    return f"{s // 60}:{s % 60:02d}"


# ─────────────────────────────────────────────────────────────────────────────
#  HTTP-клиент Ghost API
# ─────────────────────────────────────────────────────────────────────────────


class GhostClient:
    def __init__(self, base_url: str):
        self.base_url = base_url.rstrip("/")
        self.session = requests.Session()
        self.session.verify = False  # самоподписанный TLS-сертификат сервера

    def list_sessions(self):
        r = self.session.get(f"{self.base_url}/api/sessions")
        r.raise_for_status()
        return r.json()

    def get_session(self, session_id: int):
        r = self.session.get(f"{self.base_url}/api/sessions/{session_id}")
        r.raise_for_status()
        return r.json()

    def update_translation(self, utterance_id: int, text_ru: str):
        r = self.session.patch(
            f"{self.base_url}/api/utterances/{utterance_id}/translation",
            json={"text_ru": text_ru},
        )
        r.raise_for_status()


# ─────────────────────────────────────────────────────────────────────────────
#  Подготовка данных
# ─────────────────────────────────────────────────────────────────────────────


def collect_untranslated(session_detail: dict) -> list:
    """Все реплики без перевода, по порядку сегментов/индексов внутри сессии."""
    out = []
    for seg in session_detail["segments"]:
        for u in seg["utterances"]:
            if not u.get("text_ru"):
                out.append(u)
    return out


def chunked(items: list, size: int):
    for i in range(0, len(items), size):
        yield items[i : i + size]


# ─────────────────────────────────────────────────────────────────────────────
#  Промпт
# ─────────────────────────────────────────────────────────────────────────────

SYSTEM_PROMPT = (
    "Ты — профессиональный переводчик с тайского на русский. "
    "Переводишь расшифровку разговорной речи (Whisper-транскрипт чата/звонка), "
    "поэтому текст может быть разговорным, обрывочным, со сленгом и опечатками "
    "распознавания. Переводи смысл, а не подстрочник, сохраняя тон и "
    "разговорность исходной реплики. "
    "Отвечай СТРОГО JSON-массивом того же формата, что во входных данных, "
    "добавив поле \"translation\" с переводом к каждому объекту. "
    "Поля id, time, thai не менять. Никакого текста до или после JSON, "
    "никаких markdown-ограждений."
)


def build_user_prompt(chunk: list, context_pairs: list) -> str:
    parts = []

    if context_pairs:
        ctx_lines = "\n".join(f"- {th} → {ru}" for th, ru in context_pairs)
        parts.append(
            "Контекст предыдущих реплик этого же разговора "
            "(только для понимания контекста, переводить их повторно не нужно):\n"
            + ctx_lines
        )

    payload = [
        {"id": u["id"], "time": fmt_time(u["start_sec"]), "thai": u["text_thai"]}
        for u in chunk
    ]
    parts.append(
        "Переведи следующие реплики:\n"
        + json.dumps(payload, ensure_ascii=False, indent=2)
    )

    return "\n\n".join(parts)


def extract_json_array(text: str) -> list:
    """Модель иногда оборачивает ответ в ```json ... ``` или добавляет пояснения —
    вырезаем первый валидный JSON-массив из текста."""
    text = text.strip()
    text = re.sub(r"^```(?:json)?\s*", "", text)
    text = re.sub(r"\s*```$", "", text)

    start = text.find("[")
    end = text.rfind("]")
    if start == -1 or end == -1 or end < start:
        raise ValueError("В ответе модели не найден JSON-массив")

    return json.loads(text[start : end + 1])


# ─────────────────────────────────────────────────────────────────────────────
#  Локальная модель (MLX)
# ─────────────────────────────────────────────────────────────────────────────


class LocalTranslator:
    def __init__(self, model_name: str, max_tokens: int):
        from mlx_lm import generate as mlx_generate
        from mlx_lm import load

        print(f"Загружаю модель {model_name} (первый запуск может скачивать несколько ГБ)...")
        self.model, self.tokenizer = load(model_name)
        self._generate = mlx_generate
        self.max_tokens = max_tokens

    def translate_chunk(self, chunk: list, context_pairs: list) -> dict:
        user_prompt = build_user_prompt(chunk, context_pairs)
        messages = [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": user_prompt},
        ]

        if getattr(self.tokenizer, "chat_template", None):
            prompt = self.tokenizer.apply_chat_template(
                messages, tokenize=False, add_generation_prompt=True
            )
        else:
            # Фоллбэк для моделей без chat-шаблона в токенайзере
            prompt = f"{SYSTEM_PROMPT}\n\n{user_prompt}"

        raw = self._generate(
            self.model,
            self.tokenizer,
            prompt=prompt,
            max_tokens=self.max_tokens,
            verbose=False,
        )

        items = extract_json_array(raw)
        result = {}
        for item in items:
            uid = item.get("id")
            translation = item.get("translation")
            if uid is not None and translation is not None:
                result[int(uid)] = str(translation).strip()
        return result


# ─────────────────────────────────────────────────────────────────────────────
#  Основной процесс
# ─────────────────────────────────────────────────────────────────────────────


def run(args):
    client = GhostClient(args.base_url)

    if args.list:
        sessions = client.list_sessions()
        if not sessions:
            print("Сессий не найдено.")
            return
        print(f"{'ID':>4}  {'SLUG':<28} {'РЕПЛИК':>8} {'ПЕРЕВЕДЕНО':>11} {'ОСТАЛОСЬ':>9}")
        for s in sessions:
            remaining = s["utterance_count"] - s["translated_count"]
            print(
                f"{s['id']:>4}  {s['slug']:<28} {s['utterance_count']:>8} "
                f"{s['translated_count']:>11} {remaining:>9}"
            )
        return

    if args.session_id is None:
        print("Укажите --session-id (или --list для просмотра доступных сессий).", file=sys.stderr)
        sys.exit(1)

    print(f"Загружаю сессию {args.session_id}...")
    detail = client.get_session(args.session_id)
    untranslated = collect_untranslated(detail)

    if not untranslated:
        print("Все реплики этой сессии уже переведены ✓")
        return

    print(f"Сессия: {detail['slug']}  ·  непереведённых реплик: {len(untranslated)}")

    translator = LocalTranslator(args.model, args.max_tokens)

    context_pairs = []
    total_done = 0
    total_failed = 0
    chunks = list(chunked(untranslated, args.chunk_size))

    for i, chunk in enumerate(chunks, start=1):
        print(f"\nЧанк {i}/{len(chunks)}  ({len(chunk)} реплик)...")
        t0 = time.time()
        try:
            translations = translator.translate_chunk(chunk, context_pairs)
        except Exception as e:
            print(f"  ✗ Ошибка генерации/парсинга: {e}")
            total_failed += len(chunk)
            continue

        missing = [u["id"] for u in chunk if u["id"] not in translations]
        if missing:
            print(f"  ⚠ Модель не вернула перевод для id={missing}")

        for u in chunk:
            uid = u["id"]
            if uid not in translations:
                total_failed += 1
                continue

            text_ru = translations[uid]
            if not args.dry_run:
                try:
                    client.update_translation(uid, text_ru)
                except Exception as e:
                    print(f"  ✗ Не удалось сохранить id={uid}: {e}")
                    total_failed += 1
                    continue

            total_done += 1
            print(f"  [{fmt_time(u['start_sec'])}] {u['text_thai']!r} → {text_ru!r}")
            context_pairs.append((u["text_thai"], text_ru))

        context_pairs = context_pairs[-CONTEXT_WINDOW:]
        print(f"  чанк готов за {time.time() - t0:.1f}с")

    print(f"\nГотово: переведено {total_done}, ошибок {total_failed}.")
    if args.dry_run:
        print("(--dry-run: ничего не сохранялось в БД)")


def parse_args():
    p = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    p.add_argument("--session-id", type=int, default=None, help="ID сессии для перевода")
    p.add_argument("--list", action="store_true", help="Показать список сессий и выйти")
    p.add_argument(
        "--base-url", default=DEFAULT_BASE_URL,
        help=f"URL сервера Ghost (по умолчанию {DEFAULT_BASE_URL})",
    )
    p.add_argument(
        "--model", default=DEFAULT_MODEL,
        help=f"MLX-модель с Hugging Face Hub (по умолчанию {DEFAULT_MODEL})",
    )
    p.add_argument("--chunk-size", type=int, default=DEFAULT_CHUNK_SIZE, help="Реплик за один вызов модели")
    p.add_argument("--max-tokens", type=int, default=4000, help="Лимит токенов генерации на чанк")
    p.add_argument("--dry-run", action="store_true", help="Перевести, но не сохранять в БД")
    return p.parse_args()


if __name__ == "__main__":
    run(parse_args())