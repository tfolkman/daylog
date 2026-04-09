use crate::config::Config;
use crate::modules::{InsertOp, Module, SqlValue, YamlPath};
use color_eyre::eyre::Result;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Bar, BarChart, BarGroup, Block, Borders, Paragraph};
use ratatui::Frame;
use rusqlite::Connection;
use serde::Deserialize;
use yaml_rust2::Yaml;

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ClimbingConfig {
    #[serde(default)]
    pub target_hang_weight: f64,
    #[serde(default)]
    pub board_adjustments: std::collections::HashMap<String, i32>,
}

pub struct Climbing {
    pub config: ClimbingConfig,
}

impl Climbing {
    pub fn new(config: &Config) -> Self {
        let cfg: ClimbingConfig = match config.module_config("climbing") {
            Some(v) => match v.clone().try_into() {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("Warning: [climbing] config parse error: {e}. Using defaults.");
                    ClimbingConfig::default()
                }
            },
            None => ClimbingConfig::default(),
        };
        Self { config: cfg }
    }

    /// Get the board adjustment for a given board name.
    fn board_adjustment(&self, board: &str) -> i32 {
        let normalized = board.to_lowercase();
        self.config
            .board_adjustments
            .get(&normalized)
            .copied()
            .unwrap_or(0)
    }
}

/// Parse a grade string like "V5", "V5 x3", "V5x3", "v5".
/// Returns (grade_number, count).
fn parse_grade(raw: &str) -> Option<(i32, i32)> {
    let s = raw.trim().to_lowercase();
    // Split on 'x' to get optional count
    // Formats: "v5", "v5 x3", "v5x3", "v5 x 3"
    let (grade_part, count) = if let Some(idx) = s.find('x') {
        let grade_str = s[..idx].trim();
        let count_str = s[idx + 1..].trim();
        let count: i32 = count_str.parse().ok()?;
        (grade_str.to_string(), count)
    } else {
        (s.clone(), 1)
    };

    // Parse "v5" -> 5
    let grade_str = grade_part.trim();
    if !grade_str.starts_with('v') {
        return None;
    }
    let num: i32 = grade_str[1..].trim().parse().ok()?;
    Some((num, count))
}

/// Extract a list of strings from a YAML array.
fn yaml_string_list(yaml: &Yaml) -> Vec<String> {
    match yaml {
        Yaml::Array(arr) => arr
            .iter()
            .filter_map(|item| match item {
                Yaml::String(s) => Some(s.clone()),
                Yaml::Integer(i) => Some(i.to_string()),
                Yaml::Real(r) => Some(r.clone()),
                _ => None,
            })
            .collect(),
        _ => vec![],
    }
}

impl Module for Climbing {
    fn id(&self) -> &str {
        "climbing"
    }

    fn name(&self) -> &str {
        "Climbing"
    }

    fn schema(&self) -> &str {
        "CREATE TABLE IF NOT EXISTS climbs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            date TEXT NOT NULL REFERENCES days(date) ON DELETE CASCADE,
            climb_type TEXT NOT NULL,
            grade_raw TEXT NOT NULL,
            grade_number INTEGER NOT NULL,
            grade_normalized INTEGER NOT NULL,
            count INTEGER NOT NULL DEFAULT 1,
            board TEXT
        );"
    }

    fn normalize(&self, date: &str, yaml: &Yaml, _config: &Config) -> Result<Vec<InsertOp>> {
        let climbs_yaml = &yaml["climbs"];
        if climbs_yaml.is_badvalue() || climbs_yaml.is_null() {
            return Ok(vec![]);
        }

        let board = match &climbs_yaml["board"] {
            Yaml::String(s) => s.clone(),
            _ => "gym".to_string(),
        };

        let adjustment = self.board_adjustment(&board);
        let mut ops = Vec::new();

        // Process sends
        let sends = yaml_string_list(&climbs_yaml["sends"]);
        for raw in &sends {
            if let Some((grade_number, count)) = parse_grade(raw) {
                let grade_normalized = grade_number + adjustment;
                ops.push(InsertOp::Row {
                    table: "climbs",
                    columns: vec![
                        ("date", SqlValue::Text(date.to_string())),
                        ("climb_type", SqlValue::Text("send".to_string())),
                        ("grade_raw", SqlValue::Text(raw.clone())),
                        ("grade_number", SqlValue::Integer(grade_number as i64)),
                        (
                            "grade_normalized",
                            SqlValue::Integer(grade_normalized as i64),
                        ),
                        ("count", SqlValue::Integer(count as i64)),
                        ("board", SqlValue::Text(board.clone())),
                    ],
                });
            }
        }

        // Process attempts
        let attempts = yaml_string_list(&climbs_yaml["attempts"]);
        for raw in &attempts {
            if let Some((grade_number, count)) = parse_grade(raw) {
                let grade_normalized = grade_number + adjustment;
                ops.push(InsertOp::Row {
                    table: "climbs",
                    columns: vec![
                        ("date", SqlValue::Text(date.to_string())),
                        ("climb_type", SqlValue::Text("attempt".to_string())),
                        ("grade_raw", SqlValue::Text(raw.clone())),
                        ("grade_number", SqlValue::Integer(grade_number as i64)),
                        (
                            "grade_normalized",
                            SqlValue::Integer(grade_normalized as i64),
                        ),
                        ("count", SqlValue::Integer(count as i64)),
                        ("board", SqlValue::Text(board.clone())),
                    ],
                });
            }
        }

        Ok(ops)
    }

    fn draw(&self, f: &mut Frame, area: Rect, conn: &Connection, config: &Config) {
        let outer_block = Block::default().borders(Borders::ALL).title(" Climbing ");

        let inner = outer_block.inner(area);
        f.render_widget(outer_block, area);

        // Layout: pyramid (60%), weekly max (20%), session summary (20%)
        let chunks = Layout::vertical([
            Constraint::Percentage(55),
            Constraint::Percentage(25),
            Constraint::Percentage(20),
        ])
        .split(inner);

        self.draw_grade_pyramid(f, chunks[0], conn);
        self.draw_weekly_max(f, chunks[1], conn);
        self.draw_session_summary(f, chunks[2], conn, &config.effective_today());
    }

    fn status_json(&self, conn: &Connection, config: &Config) -> Option<serde_json::Value> {
        let today = config.effective_today();

        let today_sends: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(count), 0) FROM climbs WHERE climb_type='send' AND date = ?1",
                [&today],
                |row| row.get(0),
            )
            .unwrap_or(0);

        let today_attempts: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(count), 0) FROM climbs WHERE climb_type='attempt' AND date = ?1",
                [&today],
                |row| row.get(0),
            )
            .unwrap_or(0);

        let max_grade: Option<i64> = conn
            .query_row(
                "SELECT MAX(grade_normalized) FROM climbs WHERE climb_type='send' AND date = ?1",
                [&today],
                |row| row.get(0),
            )
            .unwrap_or(None);

        let board: Option<String> = conn
            .query_row(
                "SELECT board FROM climbs WHERE date = ?1 LIMIT 1",
                [&today],
                |row| row.get(0),
            )
            .ok();

        Some(serde_json::json!({
            "today_sends": today_sends,
            "today_attempts": today_attempts,
            "max_grade": max_grade.map(|g| format!("V{g}")),
            "board": board,
        }))
    }

    fn log_field_path(&self, field: &str, _subfield: &str) -> Option<YamlPath> {
        match field {
            "climb" | "send" => Some(YamlPath::ListAppend("sends".to_string())),
            "attempt" => Some(YamlPath::ListAppend("attempts".to_string())),
            _ => None,
        }
    }
}

// --- Draw helpers ---

impl Climbing {
    fn draw_grade_pyramid(&self, f: &mut Frame, area: Rect, conn: &Connection) {
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Grade Pyramid (8 wk sends) ");

        let inner = block.inner(area);
        f.render_widget(block, area);

        // Query send distribution over last 8 weeks
        let cutoff = chrono::Local::now()
            .checked_sub_signed(chrono::Duration::weeks(8))
            .map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_default();

        let rows = query_grade_distribution(conn, &cutoff);

        if rows.is_empty() {
            let msg = Paragraph::new("No climbing data in last 8 weeks");
            f.render_widget(msg, inner);
            return;
        }

        // Build horizontal bar chart
        let bars: Vec<Bar> = rows
            .iter()
            .map(|(grade, count)| {
                Bar::default()
                    .label(Line::from(format!("V{grade}")))
                    .value(*count as u64)
                    .style(grade_color(*grade))
            })
            .collect();

        let chart = BarChart::default()
            .direction(ratatui::layout::Direction::Horizontal)
            .bar_gap(0)
            .bar_width(1)
            .data(BarGroup::default().bars(&bars));

        f.render_widget(chart, inner);
    }

    fn draw_weekly_max(&self, f: &mut Frame, area: Rect, conn: &Connection) {
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Weekly Max Send (12 wk) ");

        let inner = block.inner(area);
        f.render_widget(block, area);

        let cutoff = chrono::Local::now()
            .checked_sub_signed(chrono::Duration::weeks(12))
            .map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_default();

        let weekly_maxes = query_weekly_max(conn, &cutoff);

        if weekly_maxes.is_empty() {
            let msg = Paragraph::new("No climbing data in last 12 weeks");
            f.render_widget(msg, inner);
            return;
        }

        // Build a sparkline-style text display
        let max_grade = weekly_maxes.iter().map(|(_, g)| *g).max().unwrap_or(0);
        let min_grade = weekly_maxes.iter().map(|(_, g)| *g).min().unwrap_or(0);

        let mut spans: Vec<Span> = Vec::new();
        for (week_label, grade) in &weekly_maxes {
            let color = grade_color_value(*grade);
            spans.push(Span::styled(
                format!("{week_label}:V{grade} "),
                Style::default().fg(color),
            ));
        }

        // Show range info
        let range_line = Line::from(vec![Span::styled(
            format!("Range: V{min_grade}-V{max_grade}"),
            Style::default().fg(Color::DarkGray),
        )]);

        let sparkline = Line::from(spans);
        let text = ratatui::text::Text::from(vec![sparkline, range_line]);
        let paragraph = Paragraph::new(text).wrap(ratatui::widgets::Wrap { trim: true });
        f.render_widget(paragraph, inner);
    }

    fn draw_session_summary(&self, f: &mut Frame, area: Rect, conn: &Connection, today: &str) {
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Today's Session ");

        let inner = block.inner(area);
        f.render_widget(block, area);

        let sends: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(count), 0) FROM climbs WHERE climb_type='send' AND date = ?1",
                [&today],
                |row| row.get(0),
            )
            .unwrap_or(0);

        let attempts: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(count), 0) FROM climbs WHERE climb_type='attempt' AND date = ?1",
                [&today],
                |row| row.get(0),
            )
            .unwrap_or(0);

        let max_grade: Option<i64> = conn
            .query_row(
                "SELECT MAX(grade_normalized) FROM climbs WHERE climb_type='send' AND date = ?1",
                [&today],
                |row| row.get(0),
            )
            .unwrap_or(None);

        let board: Option<String> = conn
            .query_row(
                "SELECT board FROM climbs WHERE date = ?1 LIMIT 1",
                [&today],
                |row| row.get(0),
            )
            .ok();

        let max_str = max_grade
            .map(|g| format!("V{g}"))
            .unwrap_or_else(|| "-".to_string());

        let board_str = board.unwrap_or_else(|| "-".to_string());

        let lines = vec![Line::from(vec![
            Span::styled("Sends: ", Style::default().fg(Color::DarkGray)),
            Span::styled(format!("{sends}"), Style::default().fg(Color::Green)),
            Span::raw("  "),
            Span::styled("Attempts: ", Style::default().fg(Color::DarkGray)),
            Span::styled(format!("{attempts}"), Style::default().fg(Color::Yellow)),
            Span::raw("  "),
            Span::styled("Max: ", Style::default().fg(Color::DarkGray)),
            Span::styled(max_str, Style::default().fg(Color::Cyan)),
            Span::raw("  "),
            Span::styled("Board: ", Style::default().fg(Color::DarkGray)),
            Span::styled(board_str, Style::default().fg(Color::White)),
        ])];

        let paragraph = Paragraph::new(lines);
        f.render_widget(paragraph, inner);
    }
}

// --- Query helpers ---

/// Returns Vec<(grade_normalized, total_count)> sorted by grade DESC.
fn query_grade_distribution(conn: &Connection, since: &str) -> Vec<(i64, i64)> {
    let mut stmt = match conn.prepare(
        "SELECT grade_normalized, SUM(count) as total
         FROM climbs
         WHERE climb_type = 'send' AND date >= ?1
         GROUP BY grade_normalized
         ORDER BY grade_normalized DESC",
    ) {
        Ok(s) => s,
        Err(_) => return vec![],
    };

    let rows = stmt
        .query_map([since], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })
        .ok()
        .map(|iter| iter.filter_map(|r| r.ok()).collect::<Vec<_>>())
        .unwrap_or_default();

    rows
}

/// Returns Vec<(week_label, max_grade)> for last N weeks.
fn query_weekly_max(conn: &Connection, since: &str) -> Vec<(String, i64)> {
    let mut stmt = match conn.prepare(
        "SELECT strftime('%W', date) as week_num, MAX(grade_normalized) as max_g
         FROM climbs
         WHERE climb_type = 'send' AND date >= ?1
         GROUP BY week_num
         ORDER BY week_num ASC",
    ) {
        Ok(s) => s,
        Err(_) => return vec![],
    };

    let rows = stmt
        .query_map([since], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .ok()
        .map(|iter| iter.filter_map(|r| r.ok()).collect::<Vec<_>>())
        .unwrap_or_default();

    rows.into_iter()
        .map(|(week, grade)| (format!("W{week}"), grade))
        .collect()
}

/// Map grade to a color for visual distinction.
fn grade_color(grade: i64) -> Style {
    Style::default().fg(grade_color_value(grade))
}

fn grade_color_value(grade: i64) -> Color {
    match grade {
        0..=2 => Color::Green,
        3..=4 => Color::Yellow,
        5..=6 => Color::Rgb(255, 165, 0), // Orange
        7..=8 => Color::Red,
        9..=10 => Color::Magenta,
        _ => Color::White,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Grade parsing ---

    #[test]
    fn test_parse_grade_simple() {
        assert_eq!(parse_grade("V5"), Some((5, 1)));
    }

    #[test]
    fn test_parse_grade_with_count() {
        assert_eq!(parse_grade("V4 x2"), Some((4, 2)));
    }

    #[test]
    fn test_parse_grade_no_space() {
        assert_eq!(parse_grade("V4x2"), Some((4, 2)));
    }

    #[test]
    fn test_parse_grade_case_insensitive() {
        assert_eq!(parse_grade("v5"), Some((5, 1)));
    }

    #[test]
    fn test_parse_grade_with_spaces() {
        assert_eq!(parse_grade("V5 x 3"), Some((5, 3)));
    }

    #[test]
    fn test_parse_grade_v0() {
        assert_eq!(parse_grade("V0"), Some((0, 1)));
    }

    #[test]
    fn test_parse_grade_v10() {
        assert_eq!(parse_grade("V10"), Some((10, 1)));
    }

    #[test]
    fn test_parse_grade_invalid() {
        assert_eq!(parse_grade("5.11a"), None);
        assert_eq!(parse_grade(""), None);
        assert_eq!(parse_grade("hello"), None);
    }

    // --- Board adjustments ---

    #[test]
    fn test_board_adjustment_default() {
        let climbing = Climbing {
            config: ClimbingConfig::default(),
        };
        assert_eq!(climbing.board_adjustment("gym"), 0);
        assert_eq!(climbing.board_adjustment("unknown"), 0);
    }

    #[test]
    fn test_board_adjustment_configured() {
        let mut adjustments = std::collections::HashMap::new();
        adjustments.insert("moonboard".to_string(), 3);
        adjustments.insert("kilter".to_string(), 1);
        let climbing = Climbing {
            config: ClimbingConfig {
                target_hang_weight: 0.0,
                board_adjustments: adjustments,
            },
        };
        assert_eq!(climbing.board_adjustment("moonboard"), 3);
        assert_eq!(climbing.board_adjustment("Moonboard"), 3);
        assert_eq!(climbing.board_adjustment("kilter"), 1);
        assert_eq!(climbing.board_adjustment("gym"), 0);
    }

    // --- Normalize ---

    #[test]
    fn test_normalize_basic() {
        let climbing = Climbing {
            config: ClimbingConfig::default(),
        };
        let yaml_str = "climbs:
  board: gym
  sends:
    - V5
    - V4 x2
    - V6
  attempts:
    - V7
    - V6 x3";
        let docs = yaml_rust2::YamlLoader::load_from_str(yaml_str).unwrap();
        let yaml = &docs[0];
        let config_str = "notes_dir = \"/tmp\"\n[modules]\n[exercises]\n[metrics]\n";
        let config: Config = toml::from_str(config_str).unwrap();

        let ops = climbing.normalize("2026-03-28", yaml, &config).unwrap();
        // 3 sends + 2 attempts = 5 ops
        assert_eq!(ops.len(), 5);

        // Verify first send
        match &ops[0] {
            InsertOp::Row { table, columns } => {
                assert_eq!(*table, "climbs");
                // Check climb_type
                let climb_type = columns
                    .iter()
                    .find(|(name, _)| *name == "climb_type")
                    .map(|(_, v)| v);
                assert!(matches!(climb_type, Some(SqlValue::Text(s)) if s == "send"));
                // Check grade_number
                let grade = columns
                    .iter()
                    .find(|(name, _)| *name == "grade_number")
                    .map(|(_, v)| v);
                assert!(matches!(grade, Some(SqlValue::Integer(5))));
            }
            _ => panic!("Expected InsertOp::Row"),
        }
    }

    #[test]
    fn test_normalize_with_board_adjustment() {
        let mut adjustments = std::collections::HashMap::new();
        adjustments.insert("moonboard".to_string(), 3);
        let climbing = Climbing {
            config: ClimbingConfig {
                target_hang_weight: 0.0,
                board_adjustments: adjustments,
            },
        };
        let yaml_str = "climbs:
  board: moonboard
  sends:
    - V5";
        let docs = yaml_rust2::YamlLoader::load_from_str(yaml_str).unwrap();
        let yaml = &docs[0];
        let config_str = "notes_dir = \"/tmp\"\n[modules]\n[exercises]\n[metrics]\n";
        let config: Config = toml::from_str(config_str).unwrap();

        let ops = climbing.normalize("2026-03-28", yaml, &config).unwrap();
        assert_eq!(ops.len(), 1);

        match &ops[0] {
            InsertOp::Row { columns, .. } => {
                let normalized = columns
                    .iter()
                    .find(|(name, _)| *name == "grade_normalized")
                    .map(|(_, v)| v);
                // V5 + 3 adjustment = 8
                assert!(matches!(normalized, Some(SqlValue::Integer(8))));
            }
            _ => panic!("Expected InsertOp::Row"),
        }
    }

    #[test]
    fn test_normalize_no_climbs() {
        let climbing = Climbing {
            config: ClimbingConfig::default(),
        };
        let yaml_str = "weight: 173.4";
        let docs = yaml_rust2::YamlLoader::load_from_str(yaml_str).unwrap();
        let yaml = &docs[0];
        let config_str = "notes_dir = \"/tmp\"\n[modules]\n[exercises]\n[metrics]\n";
        let config: Config = toml::from_str(config_str).unwrap();

        let ops = climbing.normalize("2026-03-28", yaml, &config).unwrap();
        assert!(ops.is_empty());
    }

    #[test]
    fn test_normalize_default_board() {
        let climbing = Climbing {
            config: ClimbingConfig::default(),
        };
        let yaml_str = "climbs:
  sends:
    - V3";
        let docs = yaml_rust2::YamlLoader::load_from_str(yaml_str).unwrap();
        let yaml = &docs[0];
        let config_str = "notes_dir = \"/tmp\"\n[modules]\n[exercises]\n[metrics]\n";
        let config: Config = toml::from_str(config_str).unwrap();

        let ops = climbing.normalize("2026-03-28", yaml, &config).unwrap();
        assert_eq!(ops.len(), 1);

        match &ops[0] {
            InsertOp::Row { columns, .. } => {
                let board = columns
                    .iter()
                    .find(|(name, _)| *name == "board")
                    .map(|(_, v)| v);
                assert!(matches!(board, Some(SqlValue::Text(s)) if s == "gym"));
            }
            _ => panic!("Expected InsertOp::Row"),
        }
    }

    #[test]
    fn test_normalize_count_preserved() {
        let climbing = Climbing {
            config: ClimbingConfig::default(),
        };
        let yaml_str = "climbs:
  sends:
    - V4 x2";
        let docs = yaml_rust2::YamlLoader::load_from_str(yaml_str).unwrap();
        let yaml = &docs[0];
        let config_str = "notes_dir = \"/tmp\"\n[modules]\n[exercises]\n[metrics]\n";
        let config: Config = toml::from_str(config_str).unwrap();

        let ops = climbing.normalize("2026-03-28", yaml, &config).unwrap();
        assert_eq!(ops.len(), 1);

        match &ops[0] {
            InsertOp::Row { columns, .. } => {
                let count = columns
                    .iter()
                    .find(|(name, _)| *name == "count")
                    .map(|(_, v)| v);
                assert!(matches!(count, Some(SqlValue::Integer(2))));
            }
            _ => panic!("Expected InsertOp::Row"),
        }
    }
}
