// src/repository.rs
//
// Слой доступа к данным — SQLite через sqlx.
//
// Принципы:
//   • Один pub struct Repository оборачивает SqlitePool.
//   • Все публичные методы принимают/возвращают типы из model.rs.
//   • SQL-запросы только здесь — сервис и хендлеры не знают про SQL.
//   • Миграции встроены в migrate() — вызывается один раз при старте.
//   • Транзакции там, где нужна атомарность (например, batch-вставка utterances).

use anyhow::{Context, Result};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions},
    Row,
};
use std::str::FromStr;

use crate::model::{
    AssignSpeaker, NewSegment, NewSession, NewSpeaker, NewUtterance, Segment,
    SegmentWithUtterances, Session, SessionDetail, Speaker, TranscriptionStatus, UpdateTranslation,
    Utterance, UtteranceWithSpeaker,
};

// ─────────────────────────────────────────────────────────────────────────────
//  Repository
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct Repository {
    pool: SqlitePool,
}

impl Repository {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Открывает (или создаёт) БД по заданному пути и прогоняет миграции.
    pub async fn open(path: &str) -> Result<Self> {
        let opts = SqliteConnectOptions::from_str(path)?
            .create_if_missing(true)
            // WAL — конкурентные чтения не блокируют записи.
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            // Синхронность NORMAL — достаточно надёжно, быстрее FULL.
            .synchronous(sqlx::sqlite::SqliteSynchronous::Normal)
            // Внешние ключи включены явно (SQLite по умолчанию выключает).
            .foreign_keys(true);

        let pool = SqlitePoolOptions::new()
            // Одна запись — SQLite не любит конкурентные писатели.
            .max_connections(1)
            .connect_with(opts)
            .await
            .context("Не удалось открыть SQLite")?;

        let repo = Self { pool };
        repo.migrate().await?;
        Ok(repo)
    }

    // ── Миграции ─────────────────────────────────────────────────────────────

    /// DDL всей схемы. Идемпотентен (IF NOT EXISTS).
    async fn migrate(&self) -> Result<()> {
        sqlx::query(
            r#"
            -- ── sessions ────────────────────────────────────────────────────
            CREATE TABLE IF NOT EXISTS sessions (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                slug        TEXT    NOT NULL UNIQUE,   -- 'rec_1781628486100'
                created_at  INTEGER NOT NULL,           -- Unix timestamp UTC
                note        TEXT
            );

            -- ── segments ────────────────────────────────────────────────────
            CREATE TABLE IF NOT EXISTS segments (
                id                    INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id            INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                idx                   INTEGER NOT NULL,  -- порядковый номер (1-based)
                filename              TEXT    NOT NULL UNIQUE,  -- 'rec_..._seg0001.webm'
                size_bytes            INTEGER,
                recorded_at           INTEGER NOT NULL,  -- Unix timestamp
                transcription_status  TEXT    NOT NULL DEFAULT 'pending'
                    CHECK(transcription_status IN ('pending','processing','done','error')),
                error_message         TEXT,              -- заполняется при status='error'
                UNIQUE(session_id, idx)
            );
            CREATE INDEX IF NOT EXISTS idx_segments_session ON segments(session_id);

            -- ── speakers ────────────────────────────────────────────────────
            CREATE TABLE IF NOT EXISTS speakers (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id  INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                label       TEXT    NOT NULL,   -- 'Оператор', 'Клиент', 'Speaker 1'
                color       TEXT                -- CSS hex: '#4f8ef7'
            );
            CREATE INDEX IF NOT EXISTS idx_speakers_session ON speakers(session_id);

            -- ── utterances ───────────────────────────────────────────────────
            -- Одна реплика = один Whisper-сегмент.
            -- speaker_id NULL → спикер не определён (сейчас всегда так).
            CREATE TABLE IF NOT EXISTS utterances (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                segment_id  INTEGER NOT NULL REFERENCES segments(id) ON DELETE CASCADE,
                idx         INTEGER NOT NULL,   -- 0-based, как у Whisper
                start_sec   REAL    NOT NULL,
                end_sec     REAL    NOT NULL,
                text_thai   TEXT    NOT NULL,
                text_ru     TEXT,               -- перевод, вводится вручную
                speaker_id  INTEGER REFERENCES speakers(id) ON DELETE SET NULL,
                confidence  REAL,               -- [0.0, 1.0]
                UNIQUE(segment_id, idx)
            );
            CREATE INDEX IF NOT EXISTS idx_utterances_segment  ON utterances(segment_id);
            CREATE INDEX IF NOT EXISTS idx_utterances_speaker  ON utterances(speaker_id);
            "#,
        )
        .execute(&self.pool)
        .await
        .context("Миграция БД провалилась")?;

        Ok(())
    }

    // ═════════════════════════════════════════════════════════════════════════
    //  Sessions
    // ═════════════════════════════════════════════════════════════════════════

    /// Создаёт новую сессию. Если slug уже существует — возвращает существующую.
    pub async fn create_session(&self, data: &NewSession) -> Result<Session> {
        // INSERT OR IGNORE + SELECT — атомарный upsert без гонок.
        let now = unix_now();
        sqlx::query("INSERT OR IGNORE INTO sessions (slug, created_at, note) VALUES (?1, ?2, ?3)")
            .bind(&data.slug)
            .bind(now)
            .bind(&data.note)
            .execute(&self.pool)
            .await?;

        self.get_session_by_slug(&data.slug)
            .await?
            .context("Сессия не найдена после вставки")
    }

    pub async fn get_session(&self, id: i64) -> Result<Option<Session>> {
        let row = sqlx::query("SELECT id, slug, created_at, note FROM sessions WHERE id = ?1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;

        Ok(row.map(|r| Session {
            id: r.get("id"),
            slug: r.get("slug"),
            created_at: r.get("created_at"),
            note: r.get("note"),
        }))
    }

    pub async fn get_session_by_slug(&self, slug: &str) -> Result<Option<Session>> {
        let row = sqlx::query("SELECT id, slug, created_at, note FROM sessions WHERE slug = ?1")
            .bind(slug)
            .fetch_optional(&self.pool)
            .await?;

        Ok(row.map(|r| Session {
            id: r.get("id"),
            slug: r.get("slug"),
            created_at: r.get("created_at"),
            note: r.get("note"),
        }))
    }

    /// Все сессии, отсортированные от новой к старой.
    pub async fn list_sessions(&self) -> Result<Vec<Session>> {
        let rows =
            sqlx::query("SELECT id, slug, created_at, note FROM sessions ORDER BY created_at DESC")
                .fetch_all(&self.pool)
                .await?;

        Ok(rows
            .into_iter()
            .map(|r| Session {
                id: r.get("id"),
                slug: r.get("slug"),
                created_at: r.get("created_at"),
                note: r.get("note"),
            })
            .collect())
    }

    /// Обновляет заметку оператора.
    pub async fn update_session_note(&self, id: i64, note: Option<&str>) -> Result<()> {
        sqlx::query("UPDATE sessions SET note = ?1 WHERE id = ?2")
            .bind(note)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn delete_session(&self, id: i64) -> Result<()> {
        sqlx::query("DELETE FROM sessions WHERE id = ?1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // ═════════════════════════════════════════════════════════════════════════
    //  Segments
    // ═════════════════════════════════════════════════════════════════════════

    pub async fn create_segment(&self, data: &NewSegment) -> Result<Segment> {
        let id = sqlx::query(
            r#"INSERT INTO segments (session_id, idx, filename, size_bytes, recorded_at, transcription_status)
               VALUES (?1, ?2, ?3, ?4, ?5, 'pending')
               RETURNING id"#,
        )
        .bind(data.session_id)
        .bind(data.index)
        .bind(&data.filename)
        .bind(data.size_bytes.map(|v| v as i64))
        .bind(data.recorded_at)
        .fetch_one(&self.pool)
        .await?
        .get::<i64, _>("id");

        self.get_segment(id)
            .await?
            .context("Сегмент не найден после вставки")
    }

    pub async fn get_segment(&self, id: i64) -> Result<Option<Segment>> {
        let row = sqlx::query(
            r#"SELECT id, session_id, idx, filename, size_bytes,
                      recorded_at, transcription_status, error_message
               FROM segments WHERE id = ?1"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(map_segment))
    }

    pub async fn get_segment_by_filename(&self, filename: &str) -> Result<Option<Segment>> {
        let row = sqlx::query(
            r#"SELECT id, session_id, idx, filename, size_bytes,
                      recorded_at, transcription_status, error_message
               FROM segments WHERE filename = ?1"#,
        )
        .bind(filename)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(map_segment))
    }

    pub async fn list_segments_for_session(&self, session_id: i64) -> Result<Vec<Segment>> {
        let rows = sqlx::query(
            r#"SELECT id, session_id, idx, filename, size_bytes,
                      recorded_at, transcription_status, error_message
               FROM segments WHERE session_id = ?1 ORDER BY idx"#,
        )
        .bind(session_id)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(map_segment).collect())
    }

    /// Все сегменты со статусом 'pending' — для очереди транскрибации.
    pub async fn list_pending_segments(&self) -> Result<Vec<Segment>> {
        let rows = sqlx::query(
            r#"SELECT id, session_id, idx, filename, size_bytes,
                      recorded_at, transcription_status, error_message
               FROM segments WHERE transcription_status = 'pending' ORDER BY recorded_at"#,
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(map_segment).collect())
    }

    pub async fn set_transcription_status(
        &self,
        segment_id: i64,
        status: TranscriptionStatus,
        error_message: Option<&str>,
    ) -> Result<()> {
        let status_str = match status {
            TranscriptionStatus::Pending => "pending",
            TranscriptionStatus::Processing => "processing",
            TranscriptionStatus::Done => "done",
            TranscriptionStatus::Error => "error",
        };
        sqlx::query(
            "UPDATE segments SET transcription_status = ?1, error_message = ?2 WHERE id = ?3",
        )
        .bind(status_str)
        .bind(error_message)
        .bind(segment_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // ═════════════════════════════════════════════════════════════════════════
    //  Speakers
    // ═════════════════════════════════════════════════════════════════════════

    pub async fn create_speaker(&self, data: &NewSpeaker) -> Result<Speaker> {
        let id = sqlx::query(
            "INSERT INTO speakers (session_id, label, color) VALUES (?1, ?2, ?3) RETURNING id",
        )
        .bind(data.session_id)
        .bind(&data.label)
        .bind(&data.color)
        .fetch_one(&self.pool)
        .await?
        .get::<i64, _>("id");

        self.get_speaker(id)
            .await?
            .context("Спикер не найден после вставки")
    }

    pub async fn get_speaker(&self, id: i64) -> Result<Option<Speaker>> {
        let row = sqlx::query("SELECT id, session_id, label, color FROM speakers WHERE id = ?1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;

        Ok(row.map(|r| Speaker {
            id: r.get("id"),
            session_id: r.get("session_id"),
            label: r.get("label"),
            color: r.get("color"),
        }))
    }

    pub async fn list_speakers_for_session(&self, session_id: i64) -> Result<Vec<Speaker>> {
        let rows = sqlx::query(
            "SELECT id, session_id, label, color FROM speakers WHERE session_id = ?1 ORDER BY id",
        )
        .bind(session_id)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| Speaker {
                id: r.get("id"),
                session_id: r.get("session_id"),
                label: r.get("label"),
                color: r.get("color"),
            })
            .collect())
    }

    pub async fn update_speaker_label(&self, id: i64, label: &str) -> Result<()> {
        sqlx::query("UPDATE speakers SET label = ?1 WHERE id = ?2")
            .bind(label)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn update_speaker_color(&self, id: i64, color: Option<&str>) -> Result<()> {
        sqlx::query("UPDATE speakers SET color = ?1 WHERE id = ?2")
            .bind(color)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn delete_speaker(&self, id: i64) -> Result<()> {
        // ON DELETE SET NULL в utterances обнулит speaker_id автоматически.
        sqlx::query("DELETE FROM speakers WHERE id = ?1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // ═════════════════════════════════════════════════════════════════════════
    //  Utterances
    // ═════════════════════════════════════════════════════════════════════════

    /// Вставляет одну реплику.
    pub async fn create_utterance(&self, data: &NewUtterance) -> Result<Utterance> {
        let id = sqlx::query(
            r#"INSERT INTO utterances
               (segment_id, idx, start_sec, end_sec, text_thai, speaker_id, confidence)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
               RETURNING id"#,
        )
        .bind(data.segment_id)
        .bind(data.index)
        .bind(data.start_sec)
        .bind(data.end_sec)
        .bind(&data.text_thai)
        .bind(data.speaker_id)
        .bind(data.confidence)
        .fetch_one(&self.pool)
        .await?
        .get::<i64, _>("id");

        self.get_utterance(id)
            .await?
            .context("Utterance не найдена после вставки")
    }

    /// Пакетная вставка реплик от Whisper — в одной транзакции.
    /// Старые реплики для этого сегмента удаляются (идемпотентно при повторной транскрибации).
    pub async fn replace_utterances_for_segment(
        &self,
        segment_id: i64,
        utterances: &[NewUtterance],
    ) -> Result<Vec<Utterance>> {
        let mut tx = self.pool.begin().await?;

        sqlx::query("DELETE FROM utterances WHERE segment_id = ?1")
            .bind(segment_id)
            .execute(&mut *tx)
            .await?;

        let mut inserted_ids: Vec<i64> = Vec::with_capacity(utterances.len());
        for u in utterances {
            let id: i64 = sqlx::query(
                r#"INSERT INTO utterances
                   (segment_id, idx, start_sec, end_sec, text_thai, speaker_id, confidence)
                   VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                   RETURNING id"#,
            )
            .bind(u.segment_id)
            .bind(u.index)
            .bind(u.start_sec)
            .bind(u.end_sec)
            .bind(&u.text_thai)
            .bind(u.speaker_id)
            .bind(u.confidence)
            .fetch_one(&mut *tx)
            .await?
            .get("id");

            inserted_ids.push(id);
        }

        tx.commit().await?;

        // Читаем вставленные записи уже через основной пул.
        let mut result = Vec::with_capacity(inserted_ids.len());
        for id in inserted_ids {
            if let Some(u) = self.get_utterance(id).await? {
                result.push(u);
            }
        }
        Ok(result)
    }

    pub async fn get_utterance(&self, id: i64) -> Result<Option<Utterance>> {
        let row = sqlx::query(
            r#"SELECT id, segment_id, idx, start_sec, end_sec,
                      text_thai, text_ru, speaker_id, confidence
               FROM utterances WHERE id = ?1"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(map_utterance))
    }

    pub async fn list_utterances_for_segment(&self, segment_id: i64) -> Result<Vec<Utterance>> {
        let rows = sqlx::query(
            r#"SELECT id, segment_id, idx, start_sec, end_sec,
                      text_thai, text_ru, speaker_id, confidence
               FROM utterances WHERE segment_id = ?1 ORDER BY idx"#,
        )
        .bind(segment_id)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(map_utterance).collect())
    }

    /// Обновляет перевод конкретной реплики. Вызывается из HTML-формы.
    pub async fn update_translation(
        &self,
        utterance_id: i64,
        data: &UpdateTranslation,
    ) -> Result<Utterance> {
        sqlx::query("UPDATE utterances SET text_ru = ?1 WHERE id = ?2")
            .bind(&data.text_ru)
            .bind(utterance_id)
            .execute(&self.pool)
            .await?;

        self.get_utterance(utterance_id)
            .await?
            .context("Utterance не найдена после обновления перевода")
    }

    /// Назначает (или снимает) спикера с реплики.
    pub async fn assign_speaker(
        &self,
        utterance_id: i64,
        data: &AssignSpeaker,
    ) -> Result<Utterance> {
        sqlx::query("UPDATE utterances SET speaker_id = ?1 WHERE id = ?2")
            .bind(data.speaker_id)
            .bind(utterance_id)
            .execute(&self.pool)
            .await?;

        self.get_utterance(utterance_id)
            .await?
            .context("Utterance не найдена после назначения спикера")
    }

    // ═════════════════════════════════════════════════════════════════════════
    //  Составные запросы (для DTO)
    // ═════════════════════════════════════════════════════════════════════════

    /// Сегмент со всеми репликами и именами спикеров — без N+1.
    pub async fn get_segment_with_utterances(
        &self,
        segment_id: i64,
    ) -> Result<Option<SegmentWithUtterances>> {
        let segment = match self.get_segment(segment_id).await? {
            Some(s) => s,
            None => return Ok(None),
        };

        // JOIN utterances LEFT JOIN speakers — один запрос.
        let rows = sqlx::query(
            r#"SELECT u.id, u.segment_id, u.idx, u.start_sec, u.end_sec,
                      u.text_thai, u.text_ru, u.speaker_id, u.confidence,
                      sp.label AS speaker_label,
                      sp.color AS speaker_color
               FROM utterances u
               LEFT JOIN speakers sp ON sp.id = u.speaker_id
               WHERE u.segment_id = ?1
               ORDER BY u.idx"#,
        )
        .bind(segment_id)
        .fetch_all(&self.pool)
        .await?;

        let utterances = rows
            .into_iter()
            .map(|r| {
                let utterance = Utterance {
                    id: r.get("id"),
                    segment_id: r.get("segment_id"),
                    index: r.get::<i64, _>("idx") as u32,
                    start_sec: r.get("start_sec"),
                    end_sec: r.get("end_sec"),
                    text_thai: r.get("text_thai"),
                    text_ru: r.get("text_ru"),
                    speaker_id: r.get("speaker_id"),
                    confidence: r.get("confidence"),
                };
                UtteranceWithSpeaker {
                    utterance,
                    speaker_label: r.get("speaker_label"),
                    speaker_color: r.get("speaker_color"),
                }
            })
            .collect();

        Ok(Some(SegmentWithUtterances {
            segment,
            utterances,
        }))
    }

    /// Полная сессия со всеми сегментами, репликами, спикерами — для главной страницы.
    pub async fn get_session_detail(&self, session_id: i64) -> Result<Option<SessionDetail>> {
        let session = match self.get_session(session_id).await? {
            Some(s) => s,
            None => return Ok(None),
        };

        let segments_raw = self.list_segments_for_session(session_id).await?;
        let speakers = self.list_speakers_for_session(session_id).await?;

        // Все реплики сессии одним запросом — без цикла.
        let utterance_rows = sqlx::query(
            r#"SELECT u.id, u.segment_id, u.idx, u.start_sec, u.end_sec,
                      u.text_thai, u.text_ru, u.speaker_id, u.confidence,
                      sp.label AS speaker_label,
                      sp.color AS speaker_color
               FROM utterances u
               JOIN segments s ON s.id = u.segment_id
               LEFT JOIN speakers sp ON sp.id = u.speaker_id
               WHERE s.session_id = ?1
               ORDER BY s.idx, u.idx"#,
        )
        .bind(session_id)
        .fetch_all(&self.pool)
        .await?;

        // Группируем реплики по segment_id.
        let mut utterances_by_seg: std::collections::HashMap<i64, Vec<UtteranceWithSpeaker>> =
            std::collections::HashMap::new();

        let mut total_thai_parts: Vec<String> = Vec::new();
        let mut total_ru_parts: Vec<String> = Vec::new();

        for r in utterance_rows {
            let seg_id: i64 = r.get("segment_id");
            let thai: String = r.get("text_thai");
            let ru: Option<String> = r.get("text_ru");

            total_thai_parts.push(thai.clone());
            if let Some(ref t) = ru {
                total_ru_parts.push(t.clone());
            }

            let utterance = Utterance {
                id: r.get("id"),
                segment_id: r.get("segment_id"),
                index: r.get::<i64, _>("idx") as u32,
                start_sec: r.get("start_sec"),
                end_sec: r.get("end_sec"),
                text_thai: thai,
                text_ru: ru,
                speaker_id: r.get("speaker_id"),
                confidence: r.get("confidence"),
            };

            utterances_by_seg
                .entry(seg_id)
                .or_default()
                .push(UtteranceWithSpeaker {
                    utterance,
                    speaker_label: r.get("speaker_label"),
                    speaker_color: r.get("speaker_color"),
                });
        }

        let segments = segments_raw
            .into_iter()
            .map(|seg| {
                let utterances = utterances_by_seg.remove(&seg.id).unwrap_or_default();
                SegmentWithUtterances {
                    segment: seg,
                    utterances,
                }
            })
            .collect();

        let total_text_thai = total_thai_parts.join("\n");
        let total_text_ru = if total_ru_parts.is_empty() {
            None
        } else {
            Some(total_ru_parts.join("\n"))
        };

        Ok(Some(SessionDetail {
            session,
            segments,
            speakers,
            total_text_thai,
            total_text_ru,
        }))
    }

    /// Список всех сессий с кратким счётчиком сегментов — для индекса.
    pub async fn list_sessions_summary(&self) -> Result<Vec<SessionSummary>> {
        let rows = sqlx::query(
            r#"SELECT
                 s.id,
                 s.slug,
                 s.created_at,
                 s.note,
                 COUNT(DISTINCT sg.id)                                   AS segment_count,
                 COUNT(DISTINCT CASE WHEN sg.transcription_status = 'done'
                                     THEN sg.id END)                     AS done_count,
                 COUNT(u.id)                                              AS utterance_count,
                 COUNT(CASE WHEN u.text_ru IS NOT NULL THEN 1 END)       AS translated_count
               FROM sessions s
               LEFT JOIN segments   sg ON sg.session_id = s.id
               LEFT JOIN utterances u  ON u.segment_id  = sg.id
               GROUP BY s.id
               ORDER BY s.created_at DESC"#,
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| SessionSummary {
                id: r.get("id"),
                slug: r.get("slug"),
                created_at: r.get("created_at"),
                note: r.get("note"),
                segment_count: r.get::<i64, _>("segment_count") as u32,
                done_count: r.get::<i64, _>("done_count") as u32,
                utterance_count: r.get::<i64, _>("utterance_count") as u32,
                translated_count: r.get::<i64, _>("translated_count") as u32,
            })
            .collect())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  SessionSummary — лёгкое DTO для индексной страницы
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, serde::Serialize)]
pub struct SessionSummary {
    pub id: i64,
    pub slug: String,
    pub created_at: i64,
    pub note: Option<String>,
    /// Всего сегментов в сессии.
    pub segment_count: u32,
    /// Сегментов с transcription_status = 'done'.
    pub done_count: u32,
    /// Всего реплик.
    pub utterance_count: u32,
    /// Реплик с заполненным text_ru.
    pub translated_count: u32,
}

// ─────────────────────────────────────────────────────────────────────────────
//  Вспомогательные маперы
// ─────────────────────────────────────────────────────────────────────────────

/// Преобразует строку БД → Segment.
fn map_segment(r: sqlx::sqlite::SqliteRow) -> Segment {
    let status_str: String = r.get("transcription_status");
    let status = match status_str.as_str() {
        "processing" => TranscriptionStatus::Processing,
        "done" => TranscriptionStatus::Done,
        "error" => TranscriptionStatus::Error,
        _ => TranscriptionStatus::Pending,
    };
    Segment {
        id: r.get("id"),
        session_id: r.get("session_id"),
        index: r.get::<i64, _>("idx") as u32,
        filename: r.get("filename"),
        size_bytes: r.get::<Option<i64>, _>("size_bytes").map(|v| v as u64),
        recorded_at: r.get("recorded_at"),
        transcription_status: status,
    }
}

/// Преобразует строку БД → Utterance.
fn map_utterance(r: sqlx::sqlite::SqliteRow) -> Utterance {
    Utterance {
        id: r.get("id"),
        segment_id: r.get("segment_id"),
        index: r.get::<i64, _>("idx") as u32,
        start_sec: r.get("start_sec"),
        end_sec: r.get("end_sec"),
        text_thai: r.get("text_thai"),
        text_ru: r.get("text_ru"),
        speaker_id: r.get("speaker_id"),
        confidence: r.get("confidence"),
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
