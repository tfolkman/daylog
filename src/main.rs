use clap::Parser;
use color_eyre::eyre::{Result, WrapErr};
use std::io::{IsTerminal, Write};

use daylog::cli::{Cli, Commands};
use daylog::config::{self, Config};
use daylog::db;
use daylog::modules;

fn main() -> Result<()> {
    color_eyre::install()?;
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Init { notes_dir, no_demo }) => cmd_init(notes_dir, no_demo),
        Some(Commands::Log { field, value }) => cmd_log(&field, &value),
        Some(Commands::Status) => cmd_status(),
        Some(Commands::Sync) => cmd_sync(),
        Some(Commands::Edit { date }) => cmd_edit(date.as_deref()),
        Some(Commands::Rebuild) => cmd_rebuild(),
        Some(Commands::Completions { shell }) => {
            daylog::cli::completions::generate(shell);
            Ok(())
        }
        None => cmd_run(),
    }
}

fn cmd_init(notes_dir_arg: Option<String>, no_demo: bool) -> Result<()> {
    let config_dir = Config::config_dir()?;
    let config_path = Config::config_path()?;

    if config_path.exists() {
        eprintln!(
            "Config already exists at {}. Delete it first to re-initialize.",
            config_path.display()
        );
        return Ok(());
    }

    // Determine notes_dir
    let notes_dir = if let Some(dir) = notes_dir_arg {
        dir
    } else if std::io::stdin().is_terminal() {
        print!("Notes directory [~/daylog-notes/]: ");
        std::io::stdout().flush()?;
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        let input = input.trim();
        if input.is_empty() {
            "~/daylog-notes".to_string()
        } else {
            input.to_string()
        }
    } else {
        "~/daylog-notes".to_string()
    };

    // Create directories
    std::fs::create_dir_all(&config_dir)?;
    let notes_path = config::expand_tilde(&notes_dir);
    std::fs::create_dir_all(&notes_path)?;

    // Write config
    let config_content = format!(
        "notes_dir = \"{}\"\n\n{}",
        notes_dir,
        config::default_config_contents()
            .lines()
            .skip(1) // skip the notes_dir line from preset
            .collect::<Vec<_>>()
            .join("\n")
    );
    std::fs::write(&config_path, config_content)?;
    eprintln!("Created {}", config_path.display());

    // Generate demo data
    if !no_demo {
        let existing_notes: Vec<_> = std::fs::read_dir(&notes_path)?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map(|ext| ext == "md").unwrap_or(false))
            .collect();

        let should_demo = if !existing_notes.is_empty() && std::io::stdin().is_terminal() {
            print!(
                "Found {} existing notes. Skip demo generation? [Y/n]: ",
                existing_notes.len()
            );
            std::io::stdout().flush()?;
            let mut input = String::new();
            std::io::stdin().read_line(&mut input)?;
            input.trim().to_lowercase() == "n"
        } else {
            existing_notes.is_empty()
        };

        if should_demo {
            let count = daylog::demo::generate_demo_data(&notes_path)?;
            eprintln!("Generated {count} days of demo data");
        }
    }

    // Run initial sync
    let config = Config::load()?;
    let registry = modules::build_registry(&config);
    let db_path = config.db_path();
    let conn = db::open_rw(&db_path)?;
    db::init_db(&conn, &registry)?;
    modules::validate_module_tables(&registry)?;
    let (synced, errors) =
        daylog::materializer::sync_all(&conn, &config.notes_dir_path(), &config, &registry)?;
    if synced > 0 {
        eprintln!("Synced {synced} notes to database");
    }
    if errors > 0 {
        eprintln!("{errors} notes had parse errors");
    }

    eprintln!("Run `daylog` to start!");
    Ok(())
}

fn cmd_log(field: &str, value: &[String]) -> Result<()> {
    let config = Config::load()?;
    let registry = modules::build_registry(&config);
    daylog::cli::log_cmd::execute(field, value, &config, &registry)
}

fn cmd_status() -> Result<()> {
    let config = Config::load()?;
    let registry = modules::build_registry(&config);
    let db_path = config.db_path();

    if !db_path.exists() {
        color_eyre::eyre::bail!(
            "Database not found at {}. Run `daylog init` or `daylog sync` first.",
            db_path.display()
        );
    }

    let conn = db::open_ro(&db_path)?;
    let today = config.effective_today();

    let mut output = serde_json::json!({});
    if let Some(day_data) = db::load_today(&conn, &today)? {
        output["today"] = day_data;
    }

    // Collect module status
    for module in &registry {
        if let Some(status) = module.status_json(&conn, &config) {
            output[module.id()] = status;
        }
    }

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

fn cmd_sync() -> Result<()> {
    let config = Config::load()?;
    let registry = modules::build_registry(&config);
    let db_path = config.db_path();
    let conn = db::open_rw(&db_path)?;
    db::init_db(&conn, &registry)?;
    modules::validate_module_tables(&registry)?;

    let (synced, errors) =
        daylog::materializer::sync_all(&conn, &config.notes_dir_path(), &config, &registry)?;
    eprintln!("Synced {synced} files ({errors} errors)");
    if errors > 0 {
        std::process::exit(1);
    }
    Ok(())
}

fn cmd_edit(date: Option<&str>) -> Result<()> {
    let config = Config::load()?;
    let date_str = match date {
        Some(d) => d.to_string(),
        None => config.effective_today(),
    };
    let note_path = config.notes_dir_path().join(format!("{date_str}.md"));

    if !note_path.exists() {
        let template = include_str!("../templates/daily-note.md");
        let content = template.replace("DATE_PLACEHOLDER", &date_str);
        std::fs::write(&note_path, content)?;
    }

    let editor = std::env::var("EDITOR")
        .or_else(|_| std::env::var("VISUAL"))
        .unwrap_or_else(|_| "vi".to_string());

    std::process::Command::new(&editor)
        .arg(&note_path)
        .status()
        .wrap_err_with(|| format!("Failed to open editor: {editor}"))?;

    Ok(())
}

fn cmd_rebuild() -> Result<()> {
    let config = Config::load()?;
    let registry = modules::build_registry(&config);
    let db_path = config.db_path();

    // Delete existing DB
    if db_path.exists() {
        std::fs::remove_file(&db_path)?;
        eprintln!("Deleted {}", db_path.display());
    }

    let conn = db::open_rw(&db_path)?;
    db::init_db(&conn, &registry)?;
    modules::validate_module_tables(&registry)?;

    let (synced, errors) =
        daylog::materializer::rebuild_all(&conn, &config.notes_dir_path(), &config, &registry)?;
    eprintln!("Rebuilt: {synced} files ({errors} errors)");
    if errors > 0 {
        std::process::exit(1);
    }
    Ok(())
}

fn cmd_run() -> Result<()> {
    daylog::app::run()
}
