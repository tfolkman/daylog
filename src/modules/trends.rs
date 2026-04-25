use crate::config::Config;
use crate::modules::{InsertOp, Module};
use color_eyre::eyre::Result;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders, Sparkline};
use ratatui::Frame;
use rusqlite::Connection;
use yaml_rust2::Yaml;

pub struct Trends {
    /// Scroll offset for when sparklines exceed the visible area.
    scroll: std::sync::atomic::AtomicUsize,
}

impl Trends {
    pub fn new(_config: &Config) -> Self {
        Self {
            scroll: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

const TREND_DAYS: i32 = 42;

impl Module for Trends {
    fn id(&self) -> &str {
        "trends"
    }

    fn name(&self) -> &str {
        "Trends"
    }

    fn normalize(&self, _date: &str, _yaml: &Yaml, _config: &Config) -> Result<Vec<InsertOp>> {
        Ok(vec![])
    }

    fn draw(&self, f: &mut Frame, area: Rect, conn: &Connection, config: &Config) {
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Trends (42d) ");

        let inner = block.inner(area);
        f.render_widget(block, area);

        // Collect all sparkline data series
        let mut series: Vec<(&str, String, Vec<u64>, Color)> = Vec::new();

        // Weight sparkline
        if let Ok(data) = load_weight_sparkline(conn) {
            if !data.is_empty() {
                let label = format!("Weight ({})", config.weight_unit);
                series.push(("", label, data, Color::Cyan));
            }
        }

        // Exercise sparklines (estimated 1RM)
        for (key, ex_config) in &config.exercises {
            if let Ok(data) = load_exercise_sparkline(conn, key) {
                if !data.is_empty() {
                    let color = super::parse_color(&ex_config.color);
                    series.push(("", ex_config.display.clone(), data, color));
                }
            }
        }

        // Metric sparklines
        for (key, m_config) in &config.metrics {
            if let Ok(data) = load_metric_sparkline(conn, key) {
                if !data.is_empty() {
                    let color = super::parse_color(&m_config.color);
                    let label = match &m_config.unit {
                        Some(u) => format!("{} ({})", m_config.display, u),
                        None => m_config.display.clone(),
                    };
                    series.push(("", label, data, color));
                }
            }
        }

        if series.is_empty() {
            let msg = ratatui::widgets::Paragraph::new("No trend data available");
            f.render_widget(msg, inner);
            return;
        }

        let sparkline_height = 3u16; // Each sparkline takes 3 rows (1 border top, 1 data, 1 border bottom)
        let visible_count = (inner.height / sparkline_height).max(1) as usize;

        let scroll = self
            .scroll
            .load(std::sync::atomic::Ordering::Relaxed)
            .min(series.len().saturating_sub(visible_count));

        let visible_series = &series[scroll..series.len().min(scroll + visible_count)];

        let constraints: Vec<Constraint> = visible_series
            .iter()
            .map(|_| Constraint::Length(sparkline_height))
            .collect();

        let chunks = Layout::vertical(constraints).split(inner);

        for (i, (_prefix, label, data, color)) in visible_series.iter().enumerate() {
            if i >= chunks.len() {
                break;
            }
            let sparkline = Sparkline::default()
                .block(
                    Block::default()
                        .borders(Borders::TOP)
                        .title(format!(" {label} ")),
                )
                .data(data)
                .style(Style::default().fg(*color));
            f.render_widget(sparkline, chunks[i]);
        }
    }

    fn keybindings(&self) -> Vec<(char, &str)> {
        vec![('j', "Scroll down"), ('k', "Scroll up")]
    }

    fn handle_key(&self, key: char, _conn: &Connection) -> bool {
        match key {
            'j' => {
                self.scroll
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                true
            }
            'k' => {
                let current = self.scroll.load(std::sync::atomic::Ordering::Relaxed);
                if current > 0 {
                    self.scroll
                        .store(current - 1, std::sync::atomic::Ordering::Relaxed);
                }
                true
            }
            _ => false,
        }
    }
}

/// Load weight data as sparkline-ready u64 values (multiplied by 10 for precision).
fn load_weight_sparkline(conn: &Connection) -> Result<Vec<u64>> {
    let trend = crate::db::load_weight_trend(conn, TREND_DAYS)?;
    if trend.is_empty() {
        return Ok(vec![]);
    }
    // Reverse so oldest is first (load_weight_trend returns DESC)
    let mut values: Vec<f64> = trend.into_iter().map(|(_, w)| w).collect();
    values.reverse();

    // Normalize to sparkline range: subtract min, multiply by 10
    let min = values.iter().cloned().fold(f64::INFINITY, f64::min);
    Ok(values.iter().map(|v| ((v - min) * 10.0) as u64).collect())
}

/// Load exercise estimated 1RM data as sparkline values.
fn load_exercise_sparkline(conn: &Connection, exercise: &str) -> Result<Vec<u64>> {
    let mut stmt = conn.prepare(
        "SELECT date, MAX(estimated_1rm) as best_e1rm
         FROM lift_sets
         WHERE exercise = ?1 AND estimated_1rm IS NOT NULL
         GROUP BY date
         ORDER BY date DESC
         LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![exercise, TREND_DAYS], |row| {
            row.get::<_, f64>(1)
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    if rows.is_empty() {
        return Ok(vec![]);
    }

    let mut values = rows;
    values.reverse();

    let min = values.iter().cloned().fold(f64::INFINITY, f64::min);
    Ok(values
        .iter()
        .map(|v| ((v - min) * 10.0) as u64 + 1)
        .collect())
}

/// Load metric data as sparkline values.
fn load_metric_sparkline(conn: &Connection, metric_name: &str) -> Result<Vec<u64>> {
    let trend = crate::db::load_metric_trend(conn, metric_name, TREND_DAYS)?;
    if trend.is_empty() {
        return Ok(vec![]);
    }

    let mut values: Vec<f64> = trend.into_iter().map(|(_, v)| v).collect();
    values.reverse();

    let min = values.iter().cloned().fold(f64::INFINITY, f64::min);
    Ok(values
        .iter()
        .map(|v| ((v - min) * 10.0) as u64 + 1)
        .collect())
}
