use crate::config::Config;
use crate::materializer::{yaml_f64_field, yaml_i32_field, yaml_str_field};
use crate::modules::{InsertOp, Module, SqlValue, YamlPath};
use color_eyre::eyre::Result;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;
use regex::Regex;
use rusqlite::Connection;
use std::sync::LazyLock;
use yaml_rust2::Yaml;

pub struct Training;

impl Training {
    pub fn new(_config: &Config) -> Self {
        Self
    }
}

// --- Lift Parsing ---

/// A single parsed set.
struct ParsedSet {
    weight_lbs: f64,
    reps: i32,
    estimated_1rm: Option<f64>,
}

/// Regex to strip parenthetical annotations like (3/3)
static RE_PAREN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\([^)]*\)").unwrap());

/// Regex to strip unit suffixes like "lbs", "lb"
static RE_UNIT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\s*(lbs?|kg)\s*").unwrap());

/// Parse a single set token like "185x5", "BWx8", "40x6x3", "BW6x3", "6"
fn parse_set_token(token: &str) -> Vec<ParsedSet> {
    let token = token.trim();
    if token.is_empty() {
        return vec![];
    }

    // Strip parenthetical annotations
    let token = RE_PAREN.replace_all(token, "").to_string();
    let token = token.trim();
    if token.is_empty() {
        return vec![];
    }

    // Strip unit suffixes
    let token = RE_UNIT.replace_all(token, "").to_string();
    let token = token.trim().to_string();

    // Handle "BW" prefix variants
    let upper = token.to_uppercase();

    // "BWx8" or "BW x8" → bodyweight x reps
    if upper.starts_with("BW") {
        let rest = &token[2..].trim_start_matches(['x', 'X', ' ']);
        // "BW6x3" → 6 reps x 3 sets
        if let Some((reps_str, sets_str)) = rest.split_once(['x', 'X']) {
            let reps = reps_str.trim().parse::<i32>().unwrap_or(0);
            let sets = sets_str.trim().parse::<i32>().unwrap_or(1);
            if reps > 0 {
                let set = ParsedSet {
                    weight_lbs: 0.0,
                    reps,
                    estimated_1rm: None,
                };
                return (0..sets)
                    .map(|_| ParsedSet {
                        weight_lbs: set.weight_lbs,
                        reps: set.reps,
                        estimated_1rm: set.estimated_1rm,
                    })
                    .collect();
            }
        }
        // "BWx8" or "BW8" → single set
        let reps = rest.trim().parse::<i32>().unwrap_or(0);
        if reps > 0 {
            return vec![ParsedSet {
                weight_lbs: 0.0,
                reps,
                estimated_1rm: None,
            }];
        }
        return vec![];
    }

    // Try "WEIGHTxREPSxSETS" (e.g., "40x6x3")
    let parts: Vec<&str> = token.split(['x', 'X']).collect();
    match parts.len() {
        3 => {
            // weight x reps x sets
            let weight = parts[0].trim().parse::<f64>().unwrap_or(0.0);
            let reps = parts[1].trim().parse::<i32>().unwrap_or(0);
            let sets = parts[2].trim().parse::<i32>().unwrap_or(1);
            if reps > 0 {
                let e1rm = epley_1rm(weight, reps);
                return (0..sets)
                    .map(|_| ParsedSet {
                        weight_lbs: weight,
                        reps,
                        estimated_1rm: e1rm,
                    })
                    .collect();
            }
        }
        2 => {
            // weight x reps
            let weight = parts[0].trim().parse::<f64>().unwrap_or(0.0);
            let reps = parts[1].trim().parse::<i32>().unwrap_or(0);
            if reps > 0 {
                return vec![ParsedSet {
                    weight_lbs: weight,
                    reps,
                    estimated_1rm: epley_1rm(weight, reps),
                }];
            }
        }
        1 => {
            // Plain number → bodyweight, N reps
            if let Ok(reps) = parts[0].trim().parse::<i32>() {
                if reps > 0 {
                    return vec![ParsedSet {
                        weight_lbs: 0.0,
                        reps,
                        estimated_1rm: None,
                    }];
                }
            }
        }
        _ => {}
    }

    vec![]
}

/// Epley formula: weight * (1 + reps/30) when weight > 0 and reps > 1
fn epley_1rm(weight: f64, reps: i32) -> Option<f64> {
    if weight > 0.0 && reps > 1 {
        Some(weight * (1.0 + reps as f64 / 30.0))
    } else if weight > 0.0 && reps == 1 {
        Some(weight)
    } else {
        None
    }
}

/// Parse a full lift value string like "185x5, 205x3, 185x5" into sets.
fn parse_lift_value(raw: &str) -> Vec<ParsedSet> {
    // Strip inline comments
    let raw = if let Some(idx) = raw.find(" #") {
        &raw[..idx]
    } else {
        raw
    };

    // Strip parenthetical annotations before slash normalization
    let raw = RE_PAREN.replace_all(raw, "").to_string();

    // Normalize slash separators to commas
    let raw = raw.replace('/', ",");

    let mut sets = Vec::new();
    for token in raw.split(',') {
        sets.extend(parse_set_token(token));
    }
    sets
}

impl Module for Training {
    fn id(&self) -> &str {
        "training"
    }

    fn name(&self) -> &str {
        "Training"
    }

    fn schema(&self) -> &str {
        "CREATE TABLE IF NOT EXISTS sessions (
            date TEXT NOT NULL REFERENCES days(date) ON DELETE CASCADE,
            session_number INTEGER NOT NULL DEFAULT 1,
            session_type TEXT,
            week INTEGER,
            block TEXT,
            duration INTEGER,
            rpe REAL,
            zone2_min INTEGER,
            hr_avg INTEGER,
            vo2_intervals TEXT,
            PRIMARY KEY (date, session_number)
        );
        CREATE TABLE IF NOT EXISTS lift_sets (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            date TEXT NOT NULL,
            session_number INTEGER NOT NULL DEFAULT 1,
            exercise TEXT NOT NULL,
            set_number INTEGER NOT NULL,
            weight_lbs REAL NOT NULL,
            reps INTEGER NOT NULL,
            estimated_1rm REAL,
            FOREIGN KEY (date, session_number) REFERENCES sessions(date, session_number) ON DELETE CASCADE
        );"
    }

    fn normalize(&self, date: &str, yaml: &Yaml, _config: &Config) -> Result<Vec<InsertOp>> {
        let mut ops = Vec::new();

        let session_type = yaml_str_field(yaml, "type");
        let week = yaml_i32_field(yaml, "week");
        let block = yaml_str_field(yaml, "block");
        let duration = yaml_i32_field(yaml, "duration");
        let rpe = yaml_f64_field(yaml, "rpe");
        let zone2_min = yaml_i32_field(yaml, "zone2_min");
        let hr_avg = yaml_i32_field(yaml, "hr_avg");
        let vo2_intervals = yaml_str_field(yaml, "vo2_intervals");

        // Only produce ops if there's any training data
        let has_training = session_type.is_some()
            || duration.is_some()
            || rpe.is_some()
            || !yaml["lifts"].is_badvalue();

        if !has_training {
            return Ok(ops);
        }

        let session_number = 1i64;

        ops.push(InsertOp::Row {
            table: "sessions",
            columns: vec![
                ("date", SqlValue::Text(date.to_string())),
                ("session_number", SqlValue::Integer(session_number)),
                (
                    "session_type",
                    match &session_type {
                        Some(s) => SqlValue::Text(s.clone()),
                        None => SqlValue::Null,
                    },
                ),
                (
                    "week",
                    match week {
                        Some(w) => SqlValue::Integer(w as i64),
                        None => SqlValue::Null,
                    },
                ),
                (
                    "block",
                    match &block {
                        Some(b) => SqlValue::Text(b.clone()),
                        None => SqlValue::Null,
                    },
                ),
                (
                    "duration",
                    match duration {
                        Some(d) => SqlValue::Integer(d as i64),
                        None => SqlValue::Null,
                    },
                ),
                (
                    "rpe",
                    match rpe {
                        Some(r) => SqlValue::Real(r),
                        None => SqlValue::Null,
                    },
                ),
                (
                    "zone2_min",
                    match zone2_min {
                        Some(z) => SqlValue::Integer(z as i64),
                        None => SqlValue::Null,
                    },
                ),
                (
                    "hr_avg",
                    match hr_avg {
                        Some(h) => SqlValue::Integer(h as i64),
                        None => SqlValue::Null,
                    },
                ),
                (
                    "vo2_intervals",
                    match &vo2_intervals {
                        Some(v) => SqlValue::Text(v.clone()),
                        None => SqlValue::Null,
                    },
                ),
            ],
        });

        // Parse lifts
        if let Yaml::Hash(ref lifts_map) = yaml["lifts"] {
            for (key, value) in lifts_map {
                let exercise = match key {
                    Yaml::String(s) => s.clone(),
                    _ => continue,
                };

                let raw_value = match value {
                    Yaml::String(s) => s.clone(),
                    Yaml::Integer(i) => i.to_string(),
                    Yaml::Real(r) => r.clone(),
                    _ => continue,
                };

                let sets = parse_lift_value(&raw_value);
                for (i, set) in sets.iter().enumerate() {
                    ops.push(InsertOp::Row {
                        table: "lift_sets",
                        columns: vec![
                            ("date", SqlValue::Text(date.to_string())),
                            ("session_number", SqlValue::Integer(session_number)),
                            ("exercise", SqlValue::Text(exercise.clone())),
                            ("set_number", SqlValue::Integer((i + 1) as i64)),
                            ("weight_lbs", SqlValue::Real(set.weight_lbs)),
                            ("reps", SqlValue::Integer(set.reps as i64)),
                            (
                                "estimated_1rm",
                                match set.estimated_1rm {
                                    Some(e) => SqlValue::Real(e),
                                    None => SqlValue::Null,
                                },
                            ),
                        ],
                    });
                }
            }
        }

        Ok(ops)
    }

    fn draw(&self, f: &mut Frame, area: Rect, conn: &Connection, config: &Config) {
        let chunks = Layout::vertical([
            Constraint::Length(5), // TSB gauge
            Constraint::Length(5), // Session metadata
            Constraint::Min(4),    // Lifts
        ])
        .split(area);

        // --- TSB Gauge ---
        draw_tsb_gauge(f, chunks[0], conn);

        // --- Session metadata ---
        let today = config.effective_today();
        draw_session_meta(f, chunks[1], conn, &today);

        // --- Today's lifts ---
        draw_lifts(f, chunks[2], conn, config, &today);
    }

    fn status_json(&self, conn: &Connection, config: &Config) -> Option<serde_json::Value> {
        let today = config.effective_today();
        let mut stmt = conn
            .prepare(
                "SELECT session_type, duration, rpe, week, block FROM sessions WHERE date = ?1",
            )
            .ok()?;

        let session = stmt
            .query_row([&today], |row| {
                Ok(serde_json::json!({
                    "session_type": row.get::<_, Option<String>>(0)?,
                    "duration": row.get::<_, Option<i32>>(1)?,
                    "rpe": row.get::<_, Option<f64>>(2)?,
                    "week": row.get::<_, Option<i32>>(3)?,
                    "block": row.get::<_, Option<String>>(4)?,
                }))
            })
            .ok()?;

        Some(session)
    }

    fn log_field_path(&self, field: &str, subfield: &str) -> Option<YamlPath> {
        match field {
            "lift" => Some(YamlPath::Nested("lifts".to_string(), subfield.to_string())),
            "session" | "type" => Some(YamlPath::Scalar("type".to_string())),
            "duration" => Some(YamlPath::Scalar("duration".to_string())),
            "rpe" => Some(YamlPath::Scalar("rpe".to_string())),
            "week" => Some(YamlPath::Scalar("week".to_string())),
            "block" => Some(YamlPath::Scalar("block".to_string())),
            _ => None,
        }
    }
}

// --- Draw helpers ---

fn draw_tsb_gauge(f: &mut Frame, area: Rect, conn: &Connection) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Training Stress Balance ");

    // Load = RPE * duration per day for last 42 days
    let loads = match load_daily_loads(conn, 42) {
        Ok(l) => l,
        Err(_) => {
            let text = Paragraph::new("No training data").block(block);
            f.render_widget(text, area);
            return;
        }
    };

    if loads.is_empty() {
        let text = Paragraph::new("No training data").block(block);
        f.render_widget(text, area);
        return;
    }

    let acute_days = loads.len().min(7);
    let acute: f64 = loads[..acute_days].iter().sum::<f64>() / 7.0;
    let chronic: f64 = loads.iter().sum::<f64>() / 42.0;
    let tsb = chronic - acute;

    let tsb_color = if tsb > 10.0 {
        Color::Green
    } else if tsb > -10.0 {
        Color::Yellow
    } else {
        Color::Red
    };

    let lines = vec![
        Line::from(vec![
            Span::raw("Acute (7d): "),
            Span::styled(format!("{acute:.0}"), Style::default().fg(Color::Cyan)),
            Span::raw("  Chronic (42d): "),
            Span::styled(format!("{chronic:.0}"), Style::default().fg(Color::Blue)),
        ]),
        Line::from(vec![
            Span::raw("TSB: "),
            Span::styled(
                format!("{tsb:+.0}"),
                Style::default().fg(tsb_color).add_modifier(Modifier::BOLD),
            ),
            Span::raw(if tsb > 10.0 {
                "  (fresh)"
            } else if tsb > -10.0 {
                "  (neutral)"
            } else {
                "  (fatigued)"
            }),
        ]),
    ];

    let text = Paragraph::new(lines).block(block);
    f.render_widget(text, area);
}

fn draw_session_meta(f: &mut Frame, area: Rect, conn: &Connection, today: &str) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Today's Session ");

    let session = conn
        .prepare("SELECT session_type, duration, rpe, week, block FROM sessions WHERE date = ?1")
        .and_then(|mut stmt| {
            stmt.query_row([today], |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<i32>>(1)?,
                    row.get::<_, Option<f64>>(2)?,
                    row.get::<_, Option<i32>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            })
        });

    let lines = match session {
        Ok((stype, dur, rpe, week, blk)) => {
            let mut parts = Vec::new();
            if let Some(t) = &stype {
                parts.push(Span::styled(
                    t.clone(),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ));
            }
            if let Some(d) = dur {
                parts.push(Span::raw(format!("  {d}min")));
            }
            if let Some(r) = rpe {
                parts.push(Span::raw(format!("  RPE {r:.0}")));
            }

            let mut line2_parts = Vec::new();
            if let Some(w) = week {
                line2_parts.push(Span::raw(format!("Week {w}")));
            }
            if let Some(b) = &blk {
                line2_parts.push(Span::raw(format!("  Block: {b}")));
            }

            let mut lines = vec![Line::from(parts)];
            if !line2_parts.is_empty() {
                lines.push(Line::from(line2_parts));
            }
            lines
        }
        Err(_) => vec![Line::from("No session data for today")],
    };

    let text = Paragraph::new(lines).block(block);
    f.render_widget(text, area);
}

fn draw_lifts(f: &mut Frame, area: Rect, conn: &Connection, config: &Config, today: &str) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Today's Lifts ");

    let lifts = conn
        .prepare(
            "SELECT exercise, set_number, weight_lbs, reps, estimated_1rm
             FROM lift_sets WHERE date = ?1 ORDER BY exercise, set_number",
        )
        .and_then(|mut stmt| {
            let rows = stmt
                .query_map([today], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i32>(1)?,
                        row.get::<_, f64>(2)?,
                        row.get::<_, i32>(3)?,
                        row.get::<_, Option<f64>>(4)?,
                    ))
                })?
                .collect::<std::result::Result<Vec<_>, _>>();
            rows
        });

    let lines: Vec<Line> = match lifts {
        Ok(rows) if !rows.is_empty() => {
            let mut lines = Vec::new();
            let mut current_exercise = String::new();

            for (exercise, _set_num, weight, reps, e1rm) in &rows {
                if *exercise != current_exercise {
                    if !current_exercise.is_empty() {
                        lines.push(Line::from(""));
                    }

                    let display_name = config
                        .exercises
                        .get(exercise.as_str())
                        .map(|e| e.display.as_str())
                        .unwrap_or(exercise.as_str());

                    let color = config
                        .exercises
                        .get(exercise.as_str())
                        .map(|e| super::parse_color(&e.color))
                        .unwrap_or(Color::White);

                    lines.push(Line::from(Span::styled(
                        display_name.to_string(),
                        Style::default().fg(color).add_modifier(Modifier::BOLD),
                    )));
                    current_exercise = exercise.clone();
                }

                let set_str = if *weight > 0.0 {
                    format!("  {weight:.0}x{reps}")
                } else {
                    format!("  BWx{reps}")
                };

                let e1rm_str = match e1rm {
                    Some(e) => format!("  (e1RM: {e:.0})"),
                    None => String::new(),
                };

                lines.push(Line::from(vec![
                    Span::raw(set_str),
                    Span::styled(e1rm_str, Style::default().fg(Color::DarkGray)),
                ]));
            }
            lines
        }
        _ => vec![Line::from("No lifts recorded today")],
    };

    let text = Paragraph::new(lines).block(block);
    f.render_widget(text, area);
}

/// Load daily training loads (RPE * duration) for the last N days, ordered most recent first.
fn load_daily_loads(conn: &Connection, days: i32) -> Result<Vec<f64>> {
    let mut stmt = conn.prepare(
        "SELECT COALESCE(rpe, 0) * COALESCE(duration, 0) as load
         FROM sessions ORDER BY date DESC LIMIT ?1",
    )?;
    let rows = stmt
        .query_map([days], |row| row.get::<_, f64>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_weighted_sets() {
        let sets = parse_lift_value("185x5, 205x3");
        assert_eq!(sets.len(), 2);
        assert_eq!(sets[0].weight_lbs, 185.0);
        assert_eq!(sets[0].reps, 5);
        assert_eq!(sets[1].weight_lbs, 205.0);
        assert_eq!(sets[1].reps, 3);
    }

    #[test]
    fn test_parse_bodyweight_sets() {
        let sets = parse_lift_value("BWx8, BWx6");
        assert_eq!(sets.len(), 2);
        assert_eq!(sets[0].weight_lbs, 0.0);
        assert_eq!(sets[0].reps, 8);
        assert_eq!(sets[1].weight_lbs, 0.0);
        assert_eq!(sets[1].reps, 6);
    }

    #[test]
    fn test_parse_expanded_sets() {
        let sets = parse_lift_value("40x6x3");
        assert_eq!(sets.len(), 3);
        for s in &sets {
            assert_eq!(s.weight_lbs, 40.0);
            assert_eq!(s.reps, 6);
        }
    }

    #[test]
    fn test_parse_bw_expanded() {
        let sets = parse_lift_value("BW6x3");
        assert_eq!(sets.len(), 3);
        for s in &sets {
            assert_eq!(s.weight_lbs, 0.0);
            assert_eq!(s.reps, 6);
        }
    }

    #[test]
    fn test_parse_unit_suffix() {
        let sets = parse_lift_value("40lbs x7");
        assert_eq!(sets.len(), 1);
        assert_eq!(sets[0].weight_lbs, 40.0);
        assert_eq!(sets[0].reps, 7);
    }

    #[test]
    fn test_parse_plain_number() {
        let sets = parse_lift_value("6");
        assert_eq!(sets.len(), 1);
        assert_eq!(sets[0].weight_lbs, 0.0);
        assert_eq!(sets[0].reps, 6);
    }

    #[test]
    fn test_parse_inline_comment() {
        let sets = parse_lift_value("185x5, 205x3 # heavy day");
        assert_eq!(sets.len(), 2);
    }

    #[test]
    fn test_parse_slash_separator() {
        let sets = parse_lift_value("185x5/205x3");
        assert_eq!(sets.len(), 2);
    }

    #[test]
    fn test_parse_parenthetical() {
        let sets = parse_lift_value("185x5 (3/3)");
        assert_eq!(sets.len(), 1);
        assert_eq!(sets[0].weight_lbs, 185.0);
        assert_eq!(sets[0].reps, 5);
    }

    #[test]
    fn test_epley_1rm() {
        // 200 * (1 + 5/30) = 200 * 1.1667 = 233.33
        let e1rm = epley_1rm(200.0, 5).unwrap();
        assert!((e1rm - 233.33).abs() < 0.1);
    }

    #[test]
    fn test_epley_1rm_bodyweight() {
        assert!(epley_1rm(0.0, 8).is_none());
    }

    #[test]
    fn test_epley_1rm_single() {
        assert_eq!(epley_1rm(315.0, 1), Some(315.0));
    }

    #[test]
    fn test_normalize_produces_ops() {
        let yaml_str = "type: lifting\nduration: 60\nrpe: 7\nweek: 2\nblock: volume\nlifts:\n  squat: 185x5, 185x5, 185x5\n  bench: 135x8, 135x8";
        let docs = yaml_rust2::YamlLoader::load_from_str(yaml_str).unwrap();
        let yaml = &docs[0];

        let config_str = "notes_dir = \"/tmp\"\n[modules]\n[exercises]\n[metrics]\n";
        let config: Config = toml::from_str(config_str).unwrap();

        let training = Training;
        let ops = training.normalize("2026-03-28", yaml, &config).unwrap();

        // 1 session + 3 squat sets + 2 bench sets = 6
        assert_eq!(ops.len(), 6);

        // First op should be the session row
        match &ops[0] {
            InsertOp::Row { table, columns } => {
                assert_eq!(*table, "sessions");
                // Find session_type column
                let stype = columns.iter().find(|(k, _)| *k == "session_type");
                assert!(stype.is_some());
            }
            _ => panic!("Expected Row op for sessions"),
        }
    }

    #[test]
    fn test_normalize_no_training_data() {
        let yaml_str = "weight: 173.4\nmood: 4";
        let docs = yaml_rust2::YamlLoader::load_from_str(yaml_str).unwrap();
        let yaml = &docs[0];

        let config_str = "notes_dir = \"/tmp\"\n[modules]\n[exercises]\n[metrics]\n";
        let config: Config = toml::from_str(config_str).unwrap();

        let training = Training;
        let ops = training.normalize("2026-03-28", yaml, &config).unwrap();
        assert!(ops.is_empty());
    }
}
