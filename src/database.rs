// src/database.rs
//
// Инициализация и конфигурация SQLite пула для всего приложения.

use anyhow::{Context, Result};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use std::str::FromStr;

/// Открывает SQLite БД и выполняет миграции.
/// Путь по умолчанию: `./transcripts.db`
pub async fn init_db(path: &str) -> Result<SqlitePool> {
    let opts = SqliteConnectOptions::from_str(path)?
        .create_if_missing(true)
        // WAL — конкурентные чтения не блокируют записи.
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        // Синхронность NORMAL — достаточно надёжно, быстрее FULL.
        .synchronous(sqlx::sqlite::SqliteSynchronous::Normal)
        // Внешние ключи включены явно.
        .foreign_keys(true);

    let pool = SqlitePoolOptions::new()
        // Одна запись — SQLite не любит конкурентные писатели.
        .max_connections(1)
        .connect_with(opts)
        .await
        .context("Не удалось открыть SQLite")?;

    // Миграции (идемпотентны — IF NOT EXISTS везде)
    run_migrations(&pool).await?;

    Ok(pool)
}

async fn run_migrations(pool: &SqlitePool) -> Result<()> {
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
    .execute(pool)
    .await
    .context("Миграция БД провалилась")?;

    Ok(())
}
