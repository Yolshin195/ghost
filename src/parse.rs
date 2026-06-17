// src/parse.rs
//
// Парсинг файлов, созданных Whisper.
// Формат: [MM:SS] текст реплики
//         [MM:SS] текст реплики

use anyhow::{Context, Result};
use regex::Regex;
use std::path::Path;

/// Одна реплика/фраза из Whisper-выводов.
#[derive(Debug, Clone)]
pub struct WhisperUtterance {
    pub start_sec: f64,
    pub end_sec: f64,
    pub text_thai: String,
}

/// Парсит .txt файл в формате Whisper.
///
/// Формат строк:
///   [00:00] เอาอ่ะ แล้วยังไงอ่ะแล้ว...
///   [00:38] ยาตัวเองโดยการที่ไป...
///
/// Вычисляет end_sec каждой реплики по start_sec следующей (или по длине аудио).
/// Если файл пуст или нет реплик → возвращает пустой вектор (не ошибка).
pub fn parse_whisper_txt(path: &Path) -> Result<Vec<WhisperUtterance>> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Не удалось прочитать {}", path.display()))?;

    parse_whisper_content(&content)
}

/// Парсит содержимое .txt как строку (удобно для тестов).
pub fn parse_whisper_content(content: &str) -> Result<Vec<WhisperUtterance>> {
    if content.trim().is_empty() {
        return Ok(Vec::new());
    }

    // Regex для парсинга строк вида "[MM:SS] текст"
    let re = Regex::new(r"^\[(\d{1,2}):(\d{2})\]\s+(.+)$").expect("Regex компилируется стабильно");

    let mut utterances: Vec<WhisperUtterance> = Vec::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        if let Some(caps) = re.captures(line) {
            let minutes: u32 = caps[1].parse().context("Некорректное число минут")?;
            let seconds: u32 = caps[2].parse().context("Некорректное число секунд")?;
            let text = caps[3].trim().to_string();

            let start_sec = (minutes as f64) * 60.0 + seconds as f64;

            utterances.push(WhisperUtterance {
                start_sec,
                end_sec: 0.0, // Пока временно, будет вычислено ниже
                text_thai: text,
            });
        }
    }

    // Вычисляем end_sec для каждой реплики
    // end_sec[i] = start_sec[i+1] (или infinity для последней)
    for i in 0..utterances.len() {
        utterances[i].end_sec = if i + 1 < utterances.len() {
            utterances[i + 1].start_sec
        } else {
            // Последняя реплика: вычисляем примерно по числу слов / средней скорости
            // Или просто берём +10 минут (большой запас).
            // Это не критично, так как в UI всё равно видны временные метки.
            estimate_end_sec(&utterances[i].text_thai)
        };
    }

    Ok(utterances)
}

/// Грубо оценивает end_sec последней реплики по количеству слов.
/// Предположение: тайский ~ 150 слов в минуту (слова разделены пробелами).
fn estimate_end_sec(text: &str) -> f64 {
    let word_count = text.split_whitespace().count() as f64;
    let estimated_duration_secs = (word_count / 150.0) * 60.0;
    estimated_duration_secs.max(5.0) // Минимум 5 секунд
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_whisper_empty() {
        let result = parse_whisper_content("").unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_parse_whisper_single_line() {
        let content = "[00:00] เอาอ่ะ แล้วยังไงอ่ะแล้ว";
        let result = parse_whisper_content(content).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].start_sec, 0.0);
        assert_eq!(result[0].text_thai, "เอาอ่ะ แล้วยังไงอ่ะแล้ว");
    }

    #[test]
    fn test_parse_whisper_multiple_lines() {
        let content = r#"[00:00] เอาอ่ะ แล้วยังไงอ่ะแล้ว
[00:38] ยาตัวเองโดยการที่ไป
[01:09] กูก็เป็นแต่ก็น้อยไง
[01:39] มันนานแล้วเหมือนกัน"#;

        let result = parse_whisper_content(content).unwrap();
        assert_eq!(result.len(), 4);

        // Проверяем первую реплику
        assert_eq!(result[0].start_sec, 0.0);
        assert_eq!(result[0].end_sec, 38.0);
        assert_eq!(result[0].text_thai, "เอาอ่ะ แล้วยังไงอ่ะแล้ว");

        // Проверяем вторую
        assert_eq!(result[1].start_sec, 38.0);
        assert_eq!(result[1].end_sec, 69.0);
        assert_eq!(result[1].text_thai, "ยาตัวเองโดยการที่ไป");

        // Проверяем последнюю (end_sec вычислена)
        assert_eq!(result[3].start_sec, 99.0);
        assert!(result[3].end_sec >= 5.0);
    }

    #[test]
    fn test_parse_whisper_with_timestamps() {
        let content = "[00:00] Hello\n[00:05] World\n[01:30] Test";
        let result = parse_whisper_content(content).unwrap();

        assert_eq!(result[0].start_sec, 0.0);
        assert_eq!(result[0].end_sec, 5.0);

        assert_eq!(result[1].start_sec, 5.0);
        assert_eq!(result[1].end_sec, 90.0); // 1:30

        assert_eq!(result[2].start_sec, 90.0);
    }

    #[test]
    fn test_parse_whisper_ignores_empty_lines() {
        let content = r#"[00:00] First


[00:10] Second"#;
        let result = parse_whisper_content(content).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].text_thai, "First");
        assert_eq!(result[1].text_thai, "Second");
    }
}
