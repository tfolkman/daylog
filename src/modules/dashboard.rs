use crate::config::Config;
use crate::modules::{InsertOp, Module};
use color_eyre::eyre::Result;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;
use rusqlite::Connection;
use yaml_rust2::Yaml;

pub struct Dashboard;

impl Dashboard {
    pub fn new(_config: &Config) -> Self {
        Self
    }
}

fn rating_color(value: i32) -> Color {
    match value {
        1 => Color::Red,
        2 => Color::LightRed,
        3 => Color::Yellow,
        4 => Color::LightGreen,
        5 => Color::Green,
        _ => Color::White,
    }
}

impl Module for Dashboard {
    fn id(&self) -> &str {
        "dashboard"
    }

    fn name(&self) -> &str {
        "Dashboard"
    }

    fn normalize(&self, _date: &str, _yaml: &Yaml, _config: &Config) -> Result<Vec<InsertOp>> {
        Ok(vec![])
    }

    fn draw(&self, f: &mut Frame, area: Rect, conn: &Connection, config: &Config) {
        let today = config.effective_today();

        let block = Block::default().borders(Borders::ALL).title(" Dashboard ");

        // Query today's vitals
        let day_data = conn
            .prepare(
                "SELECT sleep_start, sleep_end, sleep_hours, sleep_quality, mood, energy, weight
                 FROM days WHERE date = ?1",
            )
            .and_then(|mut stmt| {
                stmt.query_row([&today], |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<f64>>(2)?,
                        row.get::<_, Option<i32>>(3)?,
                        row.get::<_, Option<i32>>(4)?,
                        row.get::<_, Option<i32>>(5)?,
                        row.get::<_, Option<f64>>(6)?,
                    ))
                })
            });

        // Query session info
        let session = conn
            .prepare("SELECT session_type, week, block FROM sessions WHERE date = ?1")
            .and_then(|mut stmt| {
                stmt.query_row([&today], |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<i32>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                })
            });

        let lines: Vec<Line> = match day_data {
            Ok((sleep_start, sleep_end, sleep_hours, sleep_quality, mood, energy, weight)) => {
                let mut lines = Vec::new();

                lines.push(Line::from(Span::styled(
                    format!("Today: {today}"),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )));
                lines.push(Line::from(""));

                // Sleep
                let sleep_line = match (&sleep_start, &sleep_end, sleep_hours) {
                    (Some(start), Some(end), Some(hours)) => {
                        let quality_str = sleep_quality
                            .map(|q| format!("  quality: {q}/5"))
                            .unwrap_or_default();
                        vec![
                            Span::styled("Sleep: ", Style::default().fg(Color::Blue)),
                            Span::raw(format!("{start}-{end}  ({hours:.1}h){quality_str}")),
                        ]
                    }
                    _ => {
                        vec![
                            Span::styled("Sleep: ", Style::default().fg(Color::Blue)),
                            Span::styled("--", Style::default().fg(Color::DarkGray)),
                        ]
                    }
                };
                lines.push(Line::from(sleep_line));

                // Weight
                let weight_line = match weight {
                    Some(w) => vec![
                        Span::styled("Weight: ", Style::default().fg(Color::Blue)),
                        Span::raw(format!("{w:.1}")),
                    ],
                    None => vec![
                        Span::styled("Weight: ", Style::default().fg(Color::Blue)),
                        Span::styled("--", Style::default().fg(Color::DarkGray)),
                    ],
                };
                lines.push(Line::from(weight_line));

                // Mood
                let mood_line = match mood {
                    Some(m) => vec![
                        Span::styled("Mood: ", Style::default().fg(Color::Blue)),
                        Span::styled(format!("{m}/5"), Style::default().fg(rating_color(m))),
                    ],
                    None => vec![
                        Span::styled("Mood: ", Style::default().fg(Color::Blue)),
                        Span::styled("--", Style::default().fg(Color::DarkGray)),
                    ],
                };
                lines.push(Line::from(mood_line));

                // Energy
                let energy_line = match energy {
                    Some(e) => vec![
                        Span::styled("Energy: ", Style::default().fg(Color::Blue)),
                        Span::styled(format!("{e}/5"), Style::default().fg(rating_color(e))),
                    ],
                    None => vec![
                        Span::styled("Energy: ", Style::default().fg(Color::Blue)),
                        Span::styled("--", Style::default().fg(Color::DarkGray)),
                    ],
                };
                lines.push(Line::from(energy_line));

                // Session info
                if let Ok((stype, week, blk)) = &session {
                    lines.push(Line::from(""));
                    let mut parts =
                        vec![Span::styled("Session: ", Style::default().fg(Color::Blue))];
                    if let Some(t) = stype {
                        parts.push(Span::styled(
                            t.clone(),
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ));
                    }
                    if let Some(w) = week {
                        parts.push(Span::raw(format!("  W{w}")));
                    }
                    if let Some(b) = blk {
                        parts.push(Span::raw(format!("/{b}")));
                    }
                    lines.push(Line::from(parts));
                }

                lines
            }
            Err(_) => {
                vec![
                    Line::from(Span::styled(
                        format!("Today: {today}"),
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    )),
                    Line::from(""),
                    Line::from(Span::styled(
                        "No data for today",
                        Style::default().fg(Color::DarkGray),
                    )),
                    Line::from(Span::styled(
                        "Create a note or run `daylog edit`",
                        Style::default().fg(Color::DarkGray),
                    )),
                ]
            }
        };

        let inner = Layout::vertical([Constraint::Min(0)]).split(area);
        let text = Paragraph::new(lines).block(block);
        f.render_widget(text, inner[0]);
    }

    fn status_json(&self, conn: &Connection, config: &Config) -> Option<serde_json::Value> {
        let today = config.effective_today();
        crate::db::load_today(conn, &today).ok().flatten()
    }
}
