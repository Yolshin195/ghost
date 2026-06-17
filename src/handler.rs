// src/handler.rs
//
// HTTP обработчики для API endpoints.
// Используются как в web-сервере (main.rs), так и в bin/ingest.rs.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use std::sync::Arc;

use crate::repository::Repository;
use crate::service::{SegmentService, SessionService, UtteranceService};

// ─────────────────────────────────────────────────────────────────────────────
//  Общее состояние (SharedState)
// ─────────────────────────────────────────────────────────────────────────────

pub struct SharedState {
    pub repo: Repository,
}

impl SharedState {
    pub fn session_service(&self) -> SessionService {
        SessionService::new(self.repo.clone())
    }

    pub fn segment_service(&self) -> SegmentService {
        SegmentService::new(self.repo.clone())
    }

    pub fn utterance_service(&self) -> UtteranceService {
        UtteranceService::new(self.repo.clone())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  Обработчик ошибок
// ─────────────────────────────────────────────────────────────────────────────

pub struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: msg.into(),
        }
    }

    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: msg.into(),
        }
    }

    pub fn internal_error(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: msg.into(),
        }
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(err: anyhow::Error) -> Self {
        Self::internal_error(format!("{:#}", err))
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = serde_json::json!({
            "error": self.message
        });
        (self.status, Json(body)).into_response()
    }
}

pub type ApiResult<T> = Result<T, ApiError>;

// ─────────────────────────────────────────────────────────────────────────────
//  Handlers — Sessions
// ─────────────────────────────────────────────────────────────────────────────

/// GET /api/sessions
/// Возвращает список всех сессий с кратким резюме (счётчики файлов и репик).
pub async fn list_sessions(
    State(state): State<Arc<SharedState>>,
) -> ApiResult<Json<Vec<serde_json::Value>>> {
    let service = state.session_service();
    let summaries = service
        .list_sessions_summary()
        .await
        .map_err(|e| ApiError::internal_error(format!("Ошибка БД: {}", e)))?;

    let result: Vec<_> = summaries
        .into_iter()
        .map(|s| {
            serde_json::json!({
                "id": s.id,
                "slug": s.slug,
                "created_at": s.created_at,
                "note": s.note,
                "segment_count": s.segment_count,
                "done_count": s.done_count,
                "utterance_count": s.utterance_count,
                "translated_count": s.translated_count,
            })
        })
        .collect();

    Ok(Json(result))
}

/// GET /api/sessions/:id
/// Возвращает полную сессию со всеми сегментами и репликами.
pub async fn get_session(
    State(state): State<Arc<SharedState>>,
    Path(session_id): Path<i64>,
) -> ApiResult<Json<serde_json::Value>> {
    let service = state.session_service();
    let detail = service
        .get_session_detail(session_id)
        .await
        .map_err(|e| ApiError::internal_error(format!("Ошибка БД: {}", e)))?
        .ok_or_else(|| ApiError::not_found(format!("Сессия {session_id} не найдена")))?;

    Ok(Json(serde_json::to_value(&detail).unwrap()))
}

/// DELETE /api/sessions/:id
/// Удаляет сессию и все связанные данные.
pub async fn delete_session(
    State(state): State<Arc<SharedState>>,
    Path(session_id): Path<i64>,
) -> ApiResult<StatusCode> {
    let service = state.session_service();
    service
        .delete_session(session_id)
        .await
        .map_err(|e| ApiError::internal_error(format!("Ошибка удаления: {}", e)))?;

    Ok(StatusCode::NO_CONTENT)
}

// ─────────────────────────────────────────────────────────────────────────────
//  Handlers — Segments
// ─────────────────────────────────────────────────────────────────────────────

/// GET /api/segments/:id
/// Возвращает сегмент со всеми репликами и информацией о спикерах.
pub async fn get_segment(
    State(state): State<Arc<SharedState>>,
    Path(segment_id): Path<i64>,
) -> ApiResult<Json<serde_json::Value>> {
    let service = state.segment_service();
    let seg_with_utt = service
        .get_segment_with_utterances(segment_id)
        .await
        .map_err(|e| ApiError::internal_error(format!("Ошибка БД: {}", e)))?
        .ok_or_else(|| ApiError::not_found(format!("Сегмент {segment_id} не найден")))?;

    Ok(Json(serde_json::to_value(&seg_with_utt).unwrap()))
}

// ─────────────────────────────────────────────────────────────────────────────
//  Handlers — Utterances (переводы)
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct TranslationUpdate {
    pub text_ru: Option<String>,
}

/// PATCH /api/utterances/:id/translation
/// Обновляет перевод реплики на русский.
pub async fn update_utterance_translation(
    State(state): State<Arc<SharedState>>,
    Path(utterance_id): Path<i64>,
    Json(payload): Json<TranslationUpdate>,
) -> ApiResult<StatusCode> {
    let service = state.utterance_service();
    service
        .update_translation(utterance_id, payload.text_ru)
        .await
        .map_err(|e| ApiError::internal_error(format!("Ошибка обновления: {}", e)))?;

    Ok(StatusCode::OK)
}

/// POST /api/utterances/:id/speaker
/// Назначить спикера на реплику (для диаризации в будущем).
#[derive(Deserialize)]
pub struct AssignSpeakerPayload {
    pub speaker_id: Option<i64>,
}

pub async fn assign_speaker_to_utterance(
    State(state): State<Arc<SharedState>>,
    Path(utterance_id): Path<i64>,
    Json(payload): Json<AssignSpeakerPayload>,
) -> ApiResult<StatusCode> {
    let service = state.utterance_service();
    service
        .assign_speaker(utterance_id, payload.speaker_id)
        .await
        .map_err(|e| ApiError::internal_error(format!("Ошибка назначения: {}", e)))?;

    Ok(StatusCode::OK)
}

// ─────────────────────────────────────────────────────────────────────────────
//  Health check
// ─────────────────────────────────────────────────────────────────────────────

pub async fn health() -> StatusCode {
    StatusCode::OK
}
