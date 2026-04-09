use std::io::{self, Stdout};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use color_eyre::eyre::{Result, WrapErr};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph, Tabs};
use ratatui::Terminal;

use crate::config::Config;
use crate::modules::Module;

pub struct App {
    modules: Vec<Box<dyn Module>>,
    selected_tab: usize,
    enabled_module_ids: Vec<String>,
    should_quit: bool,
    last_refresh: Instant,
    scroll_offset: usize,
    config_changed_msg: Option<String>,
}

impl App {
    fn new(modules: Vec<Box<dyn Module>>, config: &Config) -> Self {
        let enabled_module_ids = enabled_ids(config);
        Self {
            modules,
            selected_tab: 0,
            enabled_module_ids,
            should_quit: false,
            last_refresh: Instant::now(),
            scroll_offset: 0,
            config_changed_msg: None,
        }
    }

    fn next_tab(&mut self) {
        if !self.modules.is_empty() {
            self.selected_tab = (self.selected_tab + 1) % self.modules.len();
            self.scroll_offset = 0;
        }
    }

    fn prev_tab(&mut self) {
        if !self.modules.is_empty() {
            self.selected_tab = if self.selected_tab == 0 {
                self.modules.len() - 1
            } else {
                self.selected_tab - 1
            };
            self.scroll_offset = 0;
        }
    }

    fn scroll_down(&mut self) {
        self.scroll_offset = self.scroll_offset.saturating_add(1);
    }

    fn scroll_up(&mut self) {
        self.scroll_offset = self.scroll_offset.saturating_sub(1);
    }
}

fn enabled_ids(config: &Config) -> Vec<String> {
    let all = ["dashboard", "training", "trends", "climbing"];
    all.iter()
        .filter(|id| config.is_enabled(id))
        .map(|id| id.to_string())
        .collect()
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;
    io::stdout().execute(EnableMouseCapture)?;
    let backend = CrosstermBackend::new(io::stdout());
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = io::stdout().execute(LeaveAlternateScreen);
    let _ = io::stdout().execute(DisableMouseCapture);
}

fn draw(f: &mut ratatui::Frame, app: &App, conn: &rusqlite::Connection, config: &Config) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(f.area());

    // Tab bar
    let tab_titles: Vec<Line> = app.modules.iter().map(|m| m.name().into()).collect();
    let tabs = Tabs::new(tab_titles)
        .select(app.selected_tab)
        .block(Block::default().borders(Borders::ALL).title(" daylog "))
        .highlight_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );
    f.render_widget(tabs, chunks[0]);

    // Active module content
    if let Some(module) = app.modules.get(app.selected_tab) {
        module.draw(f, chunks[1], conn, config);
    }

    // Status bar
    let status_text = if let Some(ref msg) = app.config_changed_msg {
        msg.clone()
    } else {
        " Tab: switch | j/k: scroll | e: edit | r: refresh | q: quit".to_string()
    };
    let status = Paragraph::new(status_text).style(Style::default().fg(Color::DarkGray));
    f.render_widget(status, chunks[2]);
}

fn open_editor(config: &Config) -> Result<()> {
    let today = config.effective_today();
    let note_path = config.notes_dir_path().join(format!("{today}.md"));

    if !note_path.exists() {
        let template = include_str!("../templates/daily-note.md");
        let content = template.replace("DATE_PLACEHOLDER", &today);
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

pub fn run() -> Result<()> {
    // 1. Load config
    let mut config = Config::load()?;

    // 2. Create DB + run migrations
    let db_path = config.db_path();
    let rw_conn = crate::db::open_rw(&db_path)?;

    // 3. Build module registry
    let modules = crate::modules::build_registry(&config);

    // 4. Init DB schema + validate module tables
    crate::db::init_db(&rw_conn, &modules)?;
    crate::modules::validate_module_tables(&modules)?;

    // Initial sync
    let _ = crate::materializer::sync_all(&rw_conn, &config.notes_dir_path(), &config, &modules);
    drop(rw_conn);

    // 5. Set up panic hook to restore terminal
    let default_panic = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        default_panic(info);
    }));

    // 6. Set up ctrlc handler
    let stop = Arc::new(AtomicBool::new(false));
    let stop_ctrlc = stop.clone();
    ctrlc::set_handler(move || {
        stop_ctrlc.store(true, Ordering::SeqCst);
    })?;

    // 7. Start watcher thread
    let modules_arc = Arc::new(modules);
    let watcher_handle = crate::materializer::start_watcher(
        config.notes_dir_path(),
        db_path.clone(),
        config.clone(),
        modules_arc.clone(),
        stop.clone(),
    )?;

    // Reconstruct modules for App (watcher took an Arc, we need owned Vec)
    let app_modules = crate::modules::build_registry(&config);

    // 8. Enter TUI
    let mut terminal = setup_terminal()?;

    // Open a read-only connection for drawing
    let ro_conn = crate::db::open_ro(&db_path)?;

    let mut app = App::new(app_modules, &config);
    let refresh_duration = Duration::from_secs(config.refresh_secs);

    // 9. Main loop
    loop {
        // Check ctrlc
        if stop.load(Ordering::SeqCst) {
            break;
        }

        if app.should_quit {
            break;
        }

        // Draw
        terminal.draw(|f| draw(f, &app, &ro_conn, &config))?;

        // Poll events with 250ms timeout
        if event::poll(Duration::from_millis(250))? {
            if let Event::Key(key) = event::read()? {
                // crossterm 0.28 fires Press and Release on some platforms;
                // only handle Press (or Repeat for held keys)
                if key.kind != KeyEventKind::Press && key.kind != KeyEventKind::Repeat {
                    continue;
                }

                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => {
                        app.should_quit = true;
                    }
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        app.should_quit = true;
                    }
                    KeyCode::Tab | KeyCode::Right => {
                        app.next_tab();
                    }
                    KeyCode::BackTab | KeyCode::Left => {
                        app.prev_tab();
                    }
                    KeyCode::Char('j') | KeyCode::Down => {
                        app.scroll_down();
                    }
                    KeyCode::Char('k') | KeyCode::Up => {
                        app.scroll_up();
                    }
                    KeyCode::Char('r') => {
                        app.last_refresh = Instant::now() - refresh_duration;
                    }
                    KeyCode::Char('e') => {
                        // Suspend TUI, open editor, resume
                        restore_terminal();
                        let _ = open_editor(&config);
                        terminal = setup_terminal()?;
                        // Force refresh after editing
                        app.last_refresh = Instant::now() - refresh_duration;
                    }
                    KeyCode::Char(c) => {
                        // Delegate to active module
                        if let Some(module) = app.modules.get(app.selected_tab) {
                            module.handle_key(c, &ro_conn);
                        }
                    }
                    _ => {}
                }
            }
        }

        // Config hot-reload on refresh tick
        if app.last_refresh.elapsed() >= refresh_duration {
            config = Config::load_or_keep(&config);
            let new_ids = enabled_ids(&config);
            if new_ids != app.enabled_module_ids {
                app.config_changed_msg =
                    Some(" Module change detected. Restart daylog to apply.".to_string());
            }
            app.last_refresh = Instant::now();
        }
    }

    // 10. Restore terminal, stop watcher
    restore_terminal();
    stop.store(true, Ordering::SeqCst);
    let _ = watcher_handle.join();

    Ok(())
}
