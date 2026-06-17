// src/lib.rs
//
// Главный модульный файл.
// Экспортирует все подмодули для использования в bin/ и других кратах.

pub mod database;
pub mod handler;
pub mod model;
pub mod parse;
pub mod repository;
pub mod service;

// Re-export часто используемого
pub use database::init_db;
pub use handler::{ApiError, ApiResult, SharedState};
pub use model::*;
pub use parse::{parse_whisper_content, parse_whisper_txt, WhisperUtterance};
pub use repository::Repository;
pub use service::{IngestResult, IngestService, SegmentService, SessionService, UtteranceService};
