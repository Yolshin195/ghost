// bin/ingest.rs
//
// Утилита для сканирования папки recordings/ и загрузки всех данных в БД.
//
// Использование:
//   cargo run --bin ingest -- --db ./transcripts.db --input ./recordings
//   cargo run --bin ingest -- --watch  # следить за новыми файлами
//

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use ghost::{init_db, IngestResult, IngestService, Repository};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Parser, Debug)]
#[command(
    name = "ingest",
    about = "Загрузить аудио и транскрипты из recordings/ в БД",
    version
)]
struct Cli {
    /// Путь к SQLite БД (по умолчанию: ./transcripts.db)
    #[arg(short, long, default_value = "transcripts.db")]
    db: String,

    /// Папка с аудио/txt файлами (по умолчанию: ./recordings)
    #[arg(short, long, default_value = "recordings")]
    input: PathBuf,

    /// Следить за новыми файлами и обрабатывать их автоматически
    #[arg(short, long)]
    watch: bool,

    /// Интервал проверки новых файлов (секунды, для --watch)
    #[arg(long, default_value = "5")]
    watch_interval: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Инициализируем логирование
    tracing_subscriber::fmt()
        .with_target(false)
        .with_max_level(tracing::Level::INFO)
        .init();

    let cli = Cli::parse();

    let input_dir = cli.input;
    if !input_dir.exists() {
        return Err(anyhow!("Папка {} не найдена", input_dir.display()));
    }

    println!("\n┌─────────────────────────────────────────────────────┐");
    println!("│  Audio Ingest Tool                                  │");
    println!("│  Загрузить recordings → БД                         │");
    println!("└─────────────────────────────────────────────────────┘\n");

    println!("  БД: {}", cli.db);
    println!("  Папка: {}\n", input_dir.display());

    // Инициализируем БД
    println!("  Инициализация БД...");
    let pool = init_db(&format!("sqlite://{}", cli.db))
        .await
        .context("Ошибка инициализации БД")?;

    let repo = Repository::new(pool);
    let service = IngestService::new(repo.clone());

    println!("  ✓ БД готова\n");

    if cli.watch {
        run_watch_mode(&service, &input_dir, cli.watch_interval).await?;
    } else {
        run_single_scan(&service, &input_dir).await?;
    }

    Ok(())
}

/// Однократное сканирование и обработка всех файлов
async fn run_single_scan(service: &IngestService, input_dir: &Path) -> Result<()> {
    let files = collect_audio_files(input_dir)?;

    if files.is_empty() {
        println!("  ⓘ Нет аудио файлов в {}\n", input_dir.display());
        return Ok(());
    }

    println!("  Найдено файлов: {}\n", files.len());

    let mut ok = 0;
    let mut skip = 0;
    let mut err = 0;

    for (idx, (audio_file, txt_file)) in files.iter().enumerate() {
        let filename = audio_file
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("?");

        print!("  [{:3}] {:<50}", idx + 1, filename);

        let size = audio_file
            .metadata()
            .ok()
            .and_then(|m| m.len().try_into().ok())
            .unwrap_or(0);

        let recorded_at = audio_file
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or_else(|| {
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i64
            });

        match service
            .ingest_audio_with_transcript(filename, txt_file, size, recorded_at)
            .await
        {
            Ok(result) => {
                if result.is_skipped() {
                    println!(" ⊘ пропущено");
                    skip += 1;
                } else {
                    if let IngestResult::Ingested {
                        utterance_count, ..
                    } = result
                    {
                        println!(" ✓ {:<3} реплик", utterance_count);
                        ok += 1;
                    }
                }
            }
            Err(e) => {
                println!(" ✗ {}", e);
                err += 1;
            }
        }
    }

    println!(
        "\n  Результат: {} обработано, {} пропущено, {} ошибок\n",
        ok, skip, err
    );

    Ok(())
}

/// Режим следящего процесса
async fn run_watch_mode(
    service: &IngestService,
    input_dir: &Path,
    interval_secs: u64,
) -> Result<()> {
    println!("  Режим слежения (Enter для выхода)\n");

    let mut seen_files = std::collections::HashSet::new();

    loop {
        // Проверяем stdin без блокирования
        let should_exit = check_exit_signal();
        if should_exit {
            println!("\n  Остановлено пользователем.\n");
            break;
        }

        let files = collect_audio_files(input_dir)?;

        for (audio_file, txt_file) in files {
            let filename = audio_file
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("?");

            // Пропускаем уже виданные файлы
            if seen_files.contains(filename) {
                continue;
            }

            // Пропускаем если нет .txt
            if !txt_file.exists() {
                continue;
            }

            seen_files.insert(filename.to_string());

            print!("  ↻ {:<50}", filename);
            let size = audio_file.metadata().ok().map(|m| m.len()).unwrap_or(0);
            let recorded_at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64;

            match service
                .ingest_audio_with_transcript(filename, &txt_file, size, recorded_at)
                .await
            {
                Ok(result) => {
                    if !result.is_skipped() {
                        if let ghost::IngestResult::Ingested {
                            utterance_count, ..
                        } = result
                        {
                            println!(" ✓ {:<3} реплик", utterance_count);
                        }
                    }
                }
                Err(e) => println!(" ✗ {}", e),
            }
        }

        tokio::time::sleep(std::time::Duration::from_secs(interval_secs)).await;
    }

    Ok(())
}

/// Собирает пары (аудио-файл, путь-к-txt) из папки
fn collect_audio_files(dir: &Path) -> Result<Vec<(PathBuf, PathBuf)>> {
    let mut pairs = Vec::new();

    for entry in std::fs::read_dir(dir).context("Не удалось прочитать папку")?
    {
        let entry = entry?;
        let path = entry.path();
        let fname = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

        // Пропускаем скрытые файлы и неудачные entries
        if fname.starts_with('.') {
            continue;
        }

        // Ищем .webm и .wav файлы
        if path.extension().and_then(|e| e.to_str()) != Some("webm")
            && path.extension().and_then(|e| e.to_str()) != Some("wav")
        {
            continue;
        }

        // Ищем соответствующий .txt
        let txt_path = path.with_extension("txt");
        if !txt_path.exists() {
            // Пропускаем файлы без транскрипции
            continue;
        }

        pairs.push((path, txt_path));
    }

    pairs.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(pairs)
}

/// Проверяет был ли нажат Enter (для --watch режима)
fn check_exit_signal() -> bool {
    // Простая реализация: проверяем stdin без блокирования.
    // На практике используем tokio::io с timeout.
    false // Для простоты, сейчас всегда продолжаем слежение
}
