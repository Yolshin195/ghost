// src/service.rs
//
// Слой бизнес-логики.
// Координирует работу Repository с парсингом файлов и созданием сущностей.

use anyhow::{anyhow, Context, Result};
use std::path::Path;

use crate::model::{
    NewSegment, NewSession, NewUtterance, Segment, Session, SessionDetail, TranscriptionStatus,
};
use crate::parse::parse_whisper_txt;
use crate::repository::Repository;

// ─────────────────────────────────────────────────────────────────────────────
//  SessionService — управление сессиями
// ─────────────────────────────────────────────────────────────────────────────

pub struct SessionService {
    repo: Repository,
}

impl SessionService {
    pub fn new(repo: Repository) -> Self {
        Self { repo }
    }

    /// Получить или создать сессию по slug'у.
    /// Если сессия уже существует → возвращаем существующую.
    /// Если нет → создаём новую.
    pub async fn get_or_create_session(&self, slug: &str) -> Result<Session> {
        match self.repo.get_session_by_slug(slug).await? {
            Some(s) => Ok(s),
            None => {
                let new_sess = NewSession {
                    slug: slug.to_string(),
                    note: None,
                };
                self.repo.create_session(&new_sess).await
            }
        }
    }

    /// Получить полную сессию со всеми сегментами и репликами.
    pub async fn get_session_detail(&self, session_id: i64) -> Result<Option<SessionDetail>> {
        self.repo.get_session_detail(session_id).await
    }

    /// Список всех сессий (с кратким резюме для главной страницы).
    pub async fn list_sessions_summary(&self) -> Result<Vec<crate::repository::SessionSummary>> {
        self.repo.list_sessions_summary().await
    }

    /// Удалить сессию и все связанные данные.
    pub async fn delete_session(&self, session_id: i64) -> Result<()> {
        self.repo.delete_session(session_id).await
    }

    /// Обновить заметку оператора.
    pub async fn update_note(&self, session_id: i64, note: Option<&str>) -> Result<()> {
        self.repo.update_session_note(session_id, note).await
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  SegmentService — управление сегментами и их транскрибацией
// ─────────────────────────────────────────────────────────────────────────────

pub struct SegmentService {
    repo: Repository,
}

impl SegmentService {
    pub fn new(repo: Repository) -> Self {
        Self { repo }
    }

    /// Получить сегмент по ID с полным содержимым (utterances + speakers).
    pub async fn get_segment_with_utterances(
        &self,
        segment_id: i64,
    ) -> Result<Option<crate::model::SegmentWithUtterances>> {
        self.repo.get_segment_with_utterances(segment_id).await
    }

    /// Получить сегмент по имени файла (если он уже в БД).
    pub async fn get_segment_by_filename(&self, filename: &str) -> Result<Option<Segment>> {
        self.repo.get_segment_by_filename(filename).await
    }

    /// Создать новый сегмент.
    pub async fn create_segment(&self, data: &NewSegment) -> Result<Segment> {
        self.repo.create_segment(data).await
    }

    /// Обновить статус транскрибации (используется при обработке файлов).
    pub async fn set_status(&self, segment_id: i64, status: TranscriptionStatus) -> Result<()> {
        self.repo
            .set_transcription_status(segment_id, status, None)
            .await
    }

    /// Обновить статус с сообщением об ошибке.
    pub async fn set_status_with_error(&self, segment_id: i64, error_msg: &str) -> Result<()> {
        self.repo
            .set_transcription_status(segment_id, TranscriptionStatus::Error, Some(error_msg))
            .await
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  UtteranceService — работа с репликами
// ─────────────────────────────────────────────────────────────────────────────

pub struct UtteranceService {
    repo: Repository,
}

impl UtteranceService {
    pub fn new(repo: Repository) -> Self {
        Self { repo }
    }

    /// Обновить перевод реплики (вводится вручную через UI).
    pub async fn update_translation(
        &self,
        utterance_id: i64,
        text_ru: Option<String>,
    ) -> Result<()> {
        let data = crate::model::UpdateTranslation { text_ru };
        self.repo.update_translation(utterance_id, &data).await?;
        Ok(())
    }

    /// Назначить спикера на реплику (для диаризации в будущем).
    pub async fn assign_speaker(&self, utterance_id: i64, speaker_id: Option<i64>) -> Result<()> {
        let data = crate::model::AssignSpeaker { speaker_id };
        self.repo.assign_speaker(utterance_id, &data).await?;
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  IngestService — загрузка аудио и парсинг результатов Whisper
// ─────────────────────────────────────────────────────────────────────────────

pub struct IngestService {
    repo: Repository,
}

impl IngestService {
    pub fn new(repo: Repository) -> Self {
        Self { repo }
    }

    /// Основная функция: обработать аудиофайл + txt транскрибацию.
    ///
    /// Входные данные:
    ///   - `audio_filename`: имя файла вроде «rec_1234567890_seg0001.webm»
    ///   - `txt_path`: путь к .txt файлу с результатами Whisper
    ///   - `audio_size`: размер аудиофайла в байтах
    ///   - `recorded_at`: Unix timestamp времени записи файла
    ///
    /// Логика:
    ///   1. Из имена файла парсим session_slug и segment_index
    ///   2. Проверяем: уже ли сегмент в БД?
    ///   3. Если да → пропускаем (идемпотентно)
    ///   4. Если нет → создаём Session/Segment
    ///   5. Парсим .txt файл → список Utterance'ы
    ///   6. Вставляем все Utterance'ы в одной транзакции
    ///   7. Выставляем статус сегмента = Done
    pub async fn ingest_audio_with_transcript(
        &self,
        audio_filename: &str,
        txt_path: &Path,
        audio_size: u64,
        recorded_at: i64,
    ) -> Result<IngestResult> {
        // ── Парсим имя файла ──
        let (session_slug, segment_index) = parse_audio_filename(audio_filename)?;

        // ── Проверяем: не обработан ли уже? ──
        if let Some(existing) = self.repo.get_segment_by_filename(audio_filename).await? {
            return Ok(IngestResult::Skipped {
                reason: "Сегмент уже в БД".to_string(),
                segment_id: existing.id,
            });
        }

        // ── Создаём или получаем сессию ──
        let session = self.get_or_create_session(&session_slug).await?;

        // ── Создаём сегмент ──
        let segment = self
            .repo
            .create_segment(&NewSegment {
                session_id: session.id,
                index: segment_index,
                filename: audio_filename.to_string(),
                size_bytes: Some(audio_size),
                recorded_at,
            })
            .await?;

        // ── Парсим .txt файл ──
        let utterances = match parse_whisper_txt(txt_path) {
            Ok(utts) => utts,
            Err(e) => {
                self.repo
                    .set_transcription_status(
                        segment.id,
                        TranscriptionStatus::Error,
                        Some(&format!("Ошибка парсинга txt: {}", e)),
                    )
                    .await?;
                return Err(e);
            }
        };

        let utterance_count = utterances.len();

        // ── Вставляем utterances (batch, одна транзакция) ──
        let new_utterances: Vec<NewUtterance> = utterances
            .into_iter()
            .enumerate()
            .map(|(idx, utt)| NewUtterance {
                segment_id: segment.id,
                index: idx as u32,
                start_sec: utt.start_sec,
                end_sec: utt.end_sec,
                text_thai: utt.text_thai,
                speaker_id: None, // Диаризация позже
                confidence: None, // MLX-whisper не вернул в txt-формате
            })
            .collect();

        self.repo
            .replace_utterances_for_segment(segment.id, &new_utterances)
            .await?;

        // ── Выставляем статус = Done ──
        self.repo
            .set_transcription_status(segment.id, TranscriptionStatus::Done, None)
            .await?;

        Ok(IngestResult::Ingested {
            session_slug,
            segment_index,
            segment_id: segment.id,
            utterance_count,
        })
    }

    /// Получить или создать сессию.
    async fn get_or_create_session(&self, slug: &str) -> Result<Session> {
        if let Some(sess) = self.repo.get_session_by_slug(slug).await? {
            return Ok(sess);
        }

        let new_sess = NewSession {
            slug: slug.to_string(),
            note: None,
        };
        self.repo.create_session(&new_sess).await
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  Парсинг имён файлов и результаты операций
// ─────────────────────────────────────────────────────────────────────────────

/// Парсит имя файла вроде «rec_1234567890_seg0001.webm» или «cli_1234567890_seg0001.wav»
/// Возвращает (session_slug, segment_index).
fn parse_audio_filename(filename: &str) -> Result<(String, u32)> {
    // Убираем расширение
    let stem = Path::new(filename)
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow!("Некорректное имя файла: {}", filename))?;

    // Ищем паттерн: (rec_|cli_)\d+_seg(\d+)
    // Пример: rec_1234567890_seg0001
    let parts: Vec<&str> = stem.split("_seg").collect();
    if parts.len() != 2 {
        return Err(anyhow!(
            "Имя файла должно быть вида {{rec|cli}}_TIMESTAMP_seg{{INDEX}}: {}",
            filename
        ));
    }

    let session_slug = parts[0].to_string(); // rec_1234567890 или cli_1234567890
    let segment_index: u32 = parts[1]
        .parse()
        .context("Индекс сегмента должен быть числом")?;

    Ok((session_slug, segment_index))
}

#[derive(Debug, Clone)]
pub enum IngestResult {
    Ingested {
        session_slug: String,
        segment_index: u32,
        segment_id: i64,
        utterance_count: usize,
    },
    Skipped {
        reason: String,
        segment_id: i64,
    },
}

impl IngestResult {
    pub fn is_skipped(&self) -> bool {
        matches!(self, IngestResult::Skipped { .. })
    }

    pub fn segment_id(&self) -> i64 {
        match self {
            IngestResult::Ingested { segment_id, .. } => *segment_id,
            IngestResult::Skipped { segment_id, .. } => *segment_id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_audio_filename() {
        let (slug, idx) = parse_audio_filename("rec_1781616471_seg0001.webm").unwrap();
        assert_eq!(slug, "rec_1781616471");
        assert_eq!(idx, 1);

        let (slug, idx) = parse_audio_filename("cli_1781598604_seg0015.wav").unwrap();
        assert_eq!(slug, "cli_1781598604");
        assert_eq!(idx, 15);
    }
}
