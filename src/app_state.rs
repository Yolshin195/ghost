// src/app_state.rs
//
// Инициализация общего состояния приложения (AppState).
// Вынесено из main.rs, чтобы не смешивать конфигурацию и бизнес-логику.
//
// Использование:
//   let state = AppState::init("sqlite://transcripts.db").await?;
//   let router = Router::new().with_state(Arc::new(state));

use anyhow::{Context, Result};
use std::sync::Arc;

use crate::handler::SharedState;
use crate::repository::Repository;

/// Инициализирует подключение к БД и возвращает готовый SharedState.
///
/// `db_url` — строка подключения SQLite, например `"sqlite://transcripts.db"`.
/// БД будет создана автоматически если её нет (CREATE IF NOT EXISTS).
/// Миграции накатываются при каждом запуске (идемпотентно).
pub async fn build_shared_state(db_url: &str) -> Result<Arc<SharedState>> {
    tracing::info!("Подключение к БД: {db_url}");

    let repo = Repository::open(db_url)
        .await
        .with_context(|| format!("Не удалось открыть БД: {db_url}"))?;

    tracing::info!("БД готова, миграции применены");

    Ok(Arc::new(SharedState { repo }))
}