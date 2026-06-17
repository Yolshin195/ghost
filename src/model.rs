// src/model.rs
//
// Модель данных для аудио-транскрибатора.
//
// Иерархия:
//   Session          — одна запись (одно нажатие «Старт»)
//     └── Segment    — один аудио-файл (2-минутный кусок)
//           └── Utterance — одна реплика / фраза внутри сегмента
//
// Принципы:
//   • Все id — i64 (rowid SQLite, проще всего).
//   • Временны́е метки — f64 секунды от начала сегмента (как у Whisper).
//   • speaker_id — Option, чтобы модель работала и без диаризации.
//   • Перевод — Option, пока не введён вручную.
//   • Все поля pub — модель чисто дата-класс, логика в service.rs.

use serde::{Deserialize, Serialize};

// ═══════════════════════════════════════════════════════════════════
//  Session
// ═══════════════════════════════════════════════════════════════════

/// Одна запись (одна сессия браузера или консольного клиента).
/// id — «rec_XXXXXXXXXX» или «cli_XXXXXXXXXX» (как сейчас в файловых именах).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    /// Первичный ключ, rowid SQLite.
    pub id: i64,

    /// Человекочитаемый идентификатор: «rec_1781628486100».
    /// Совпадает с префиксом имён файлов — так всегда можно найти аудио.
    pub slug: String,

    /// Метка времени начала сессии (Unix timestamp, UTC).
    pub created_at: i64,

    /// Произвольная заметка оператора (опционально).
    pub note: Option<String>,
}

/// Данные для создания новой сессии (без id и created_at).
#[derive(Debug, Deserialize)]
pub struct NewSession {
    pub slug: String,
    pub note: Option<String>,
}

// ═══════════════════════════════════════════════════════════════════
//  Segment
// ═══════════════════════════════════════════════════════════════════

/// Один аудио-файл внутри сессии (≈2 минуты).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Segment {
    pub id: i64,

    /// FK → Session.id
    pub session_id: i64,

    /// Порядковый номер внутри сессии (1, 2, 3, …).
    pub index: u32,

    /// Имя файла без пути: «rec_1781628486100_seg0001.webm».
    /// По нему всегда можно найти файл в папке recordings/.
    pub filename: String,

    /// Размер файла в байтах (заполняется при сохранении).
    pub size_bytes: Option<u64>,

    /// Unix timestamp момента, когда файл был записан на диск.
    pub recorded_at: i64,

    /// Статус транскрибации.
    pub transcription_status: TranscriptionStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptionStatus {
    /// Файл сохранён, транскрибация ещё не запускалась.
    Pending,
    /// Whisper сейчас обрабатывает этот файл.
    Processing,
    /// Транскрибация завершена успешно.
    Done,
    /// Ошибка транскрибации (сообщение в поле error_message).
    Error,
}

/// Данные для создания нового сегмента.
#[derive(Debug, Deserialize)]
pub struct NewSegment {
    pub session_id: i64,
    pub index: u32,
    pub filename: String,
    pub size_bytes: Option<u64>,
    pub recorded_at: i64,
}

// ═══════════════════════════════════════════════════════════════════
//  Speaker  (для будущей диаризации)
// ═══════════════════════════════════════════════════════════════════

/// Спикер — участник разговора.
/// Пока создаётся вручную (или автоматически как «Speaker 1», «Speaker 2»).
/// Потом будет заполняться pyannote / диаризатором.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Speaker {
    pub id: i64,

    /// FK → Session.id.
    /// Спикеры привязаны к сессии: один и тот же человек в двух сессиях —
    /// два разных Speaker (пока нет cross-session identity).
    pub session_id: i64,

    /// Метка спикера для отображения: «Оператор», «Клиент», «Speaker 1».
    pub label: String,

    /// Цвет для подсветки в UI (CSS hex, например «#4f8ef7»).
    pub color: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct NewSpeaker {
    pub session_id: i64,
    pub label: String,
    pub color: Option<String>,
}

// ═══════════════════════════════════════════════════════════════════
//  Utterance
// ═══════════════════════════════════════════════════════════════════

/// Одна реплика / фраза.
/// Whisper возвращает список сегментов с временными метками —
/// каждый такой сегмент становится одним Utterance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Utterance {
    pub id: i64,

    /// FK → Segment.id
    pub segment_id: i64,

    /// Порядковый номер реплики внутри сегмента (0-based, как у Whisper).
    pub index: u32,

    /// Начало реплики в секундах от начала сегмента.
    pub start_sec: f64,

    /// Конец реплики в секундах от начала сегмента.
    pub end_sec: f64,

    /// Оригинальный тайский текст от Whisper.
    pub text_thai: String,

    /// Перевод на русский — вводится вручную через UI.
    pub text_ru: Option<String>,

    /// FK → Speaker.id — заполняется диаризатором или вручную.
    /// None = спикер неизвестен (сейчас всегда None).
    pub speaker_id: Option<i64>,

    /// Уверенность Whisper [0.0, 1.0]. None если модель не вернула.
    pub confidence: Option<f64>,
}

/// Данные для создания utterance (из результатов Whisper).
#[derive(Debug, Deserialize)]
pub struct NewUtterance {
    pub segment_id: i64,
    pub index: u32,
    pub start_sec: f64,
    pub end_sec: f64,
    pub text_thai: String,
    pub speaker_id: Option<i64>,
    pub confidence: Option<f64>,
}

/// Данные для обновления перевода (из HTML-формы).
#[derive(Debug, Deserialize)]
pub struct UpdateTranslation {
    pub text_ru: Option<String>,
}

/// Данные для назначения спикера на реплику.
#[derive(Debug, Deserialize)]
pub struct AssignSpeaker {
    pub speaker_id: Option<i64>,
}

// ═══════════════════════════════════════════════════════════════════
//  Составные DTO (для API-ответов)
// ═══════════════════════════════════════════════════════════════════

/// Полный сегмент с репликами — для страницы транскрипта.
#[derive(Debug, Serialize)]
pub struct SegmentWithUtterances {
    #[serde(flatten)]
    pub segment: Segment,
    pub utterances: Vec<UtteranceWithSpeaker>,
}

/// Реплика с именем спикера — чтобы не делать N+1 запросов в UI.
#[derive(Debug, Serialize)]
pub struct UtteranceWithSpeaker {
    #[serde(flatten)]
    pub utterance: Utterance,
    /// Имя спикера (если назначен).
    pub speaker_label: Option<String>,
    /// Цвет спикера (если назначен).
    pub speaker_color: Option<String>,
}

/// Полная сессия с сегментами — для списка на главной.
#[derive(Debug, Serialize)]
pub struct SessionDetail {
    #[serde(flatten)]
    pub session: Session,
    pub segments: Vec<SegmentWithUtterances>,
    pub speakers: Vec<Speaker>,
    /// Весь тайский текст сессии одной строкой (для быстрого копирования).
    pub total_text_thai: String,
    /// Весь русский перевод одной строкой (None если ни одной реплики не переведено).
    pub total_text_ru: Option<String>,
}
