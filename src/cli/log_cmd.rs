use chrono::Local;
use color_eyre::eyre::{bail, Result};

use crate::config::Config;
use crate::frontmatter;
use crate::modules::{Module, YamlPath};

/// Execute the `daylog log <field> <value...>` command.
///
/// Writes a value to today's daily note frontmatter.
pub fn execute(
    field: &str,
    value: &[String],
    config: &Config,
    modules: &[Box<dyn Module>],
) -> Result<()> {
    let joined = value.join(" ");
    if joined.is_empty() {
        bail!("No value provided for field '{field}'");
    }

    let today = Local::now().format("%Y-%m-%d").to_string();
    let note_path = config.notes_dir_path().join(format!("{today}.md"));

    // Read or create today's note
    let content = if note_path.exists() {
        std::fs::read_to_string(&note_path)?
    } else {
        crate::template::render_daily_note(&today, config)
    };

    // Apply the edit based on field routing
    let updated = route_field(field, value, &joined, &content, config, modules)?;

    // Write atomically
    frontmatter::atomic_write(&note_path, &updated)?;
    eprintln!("Updated {today}");
    Ok(())
}

/// Validate a core field value before writing.
fn validate_core_field(field: &str, value: &str, config: &Config) -> Result<()> {
    match field {
        "weight" => {
            let unit = config.weight_unit;
            let w: f64 = value.parse().map_err(|_| {
                color_eyre::eyre::eyre!(
                    "Invalid weight: '{value}'. Expected a number in {unit} (e.g., 173.4)"
                )
            })?;
            if w <= 0.0 || w > 1000.0 {
                bail!("Invalid weight: {w}. Expected a value between 0 and 1000 {unit}");
            }
        }
        "mood" | "energy" | "sleep_quality" => {
            let n: i32 = value
                .parse()
                .map_err(|_| color_eyre::eyre::eyre!("Invalid {field}: '{value}'. Expected 1-5"))?;
            if !(1..=5).contains(&n) {
                bail!("Invalid {field}: {n}. Expected 1-5");
            }
        }
        "sleep" => {
            // Must contain a dash separating start-end times
            if !value.contains('-') {
                bail!("Invalid sleep format: '{value}'. Expected start-end (e.g., 10:30pm-6:15am)");
            }
        }
        _ => {}
    }
    Ok(())
}

/// Route a field name to the correct frontmatter edit operation.
fn route_field(
    field: &str,
    value: &[String],
    joined: &str,
    content: &str,
    config: &Config,
    modules: &[Box<dyn Module>],
) -> Result<String> {
    // Core fields: validate then write
    match field {
        "weight" => {
            validate_core_field("weight", joined, config)?;
            return Ok(frontmatter::set_scalar(content, "weight", joined));
        }
        "sleep" => {
            validate_core_field("sleep", joined, config)?;
            return Ok(frontmatter::set_scalar(
                content,
                "sleep",
                &format!("\"{}\"", joined),
            ));
        }
        "mood" => {
            validate_core_field("mood", joined, config)?;
            return Ok(frontmatter::set_scalar(content, "mood", joined));
        }
        "energy" => {
            validate_core_field("energy", joined, config)?;
            return Ok(frontmatter::set_scalar(content, "energy", joined));
        }
        "sleep_quality" => {
            validate_core_field("sleep_quality", joined, config)?;
            return Ok(frontmatter::set_scalar(content, "sleep_quality", joined));
        }
        _ => {}
    }

    // Special case: metric field
    if field == "metric" {
        if value.len() < 2 {
            bail!("Usage: daylog log metric <name> <value>");
        }
        let subfield = &value[0];
        let remaining = value[1..].join(" ");
        // Validate metric value is numeric
        remaining.parse::<f64>().map_err(|_| {
            color_eyre::eyre::eyre!("Invalid metric value: '{remaining}'. Expected a number")
        })?;
        return Ok(frontmatter::set_scalar(content, subfield, &remaining));
    }

    // Module fields: extract first token as potential subfield
    let first_token = value.first().map(|s| s.as_str()).unwrap_or("");

    for module in modules {
        if let Some(yaml_path) = module.log_field_path(field, first_token) {
            return match yaml_path {
                YamlPath::Scalar(key) => Ok(frontmatter::set_scalar(content, &key, joined)),
                YamlPath::Nested(parent, child) => {
                    let remaining = value[1..].join(" ");
                    if remaining.is_empty() {
                        bail!("Usage: daylog log {field} <subfield> <value>");
                    }
                    Ok(frontmatter::set_nested(
                        content, &parent, &child, &remaining,
                    ))
                }
                YamlPath::ListAppend(key) => Ok(frontmatter::append_to_list(content, &key, joined)),
            };
        }
    }

    bail!("Unknown field '{field}'. Available: weight, sleep, mood, energy, lift, climb, metric")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "---
date: 2026-03-28
sleep: \"10:30pm-6:15am\"
weight: 173.4
mood: 4
lifts:
  squat: 185x5, 185x5
  bench: 135x8
---

## Notes

Good session.
";

    fn empty_modules() -> Vec<Box<dyn Module>> {
        vec![]
    }

    fn default_config() -> Config {
        test_config()
    }

    // -- Core field routing tests --

    #[test]
    fn test_route_weight() {
        let cfg = default_config();
        let value = vec!["175.0".to_string()];
        let result =
            route_field("weight", &value, "175.0", SAMPLE, &cfg, &empty_modules()).unwrap();
        assert!(result.contains("weight: 175.0"));
        assert!(!result.contains("173.4"));
    }

    #[test]
    fn test_route_sleep() {
        let cfg = default_config();
        let value = vec!["11pm-7am".to_string()];
        let result =
            route_field("sleep", &value, "11pm-7am", SAMPLE, &cfg, &empty_modules()).unwrap();
        assert!(result.contains("sleep: \"11pm-7am\""));
    }

    #[test]
    fn test_route_mood() {
        let cfg = default_config();
        let value = vec!["5".to_string()];
        let result = route_field("mood", &value, "5", SAMPLE, &cfg, &empty_modules()).unwrap();
        assert!(result.contains("mood: 5"));
        assert!(!result.contains("mood: 4"));
    }

    #[test]
    fn test_route_energy() {
        let cfg = default_config();
        let value = vec!["3".to_string()];
        let result = route_field("energy", &value, "3", SAMPLE, &cfg, &empty_modules()).unwrap();
        assert!(result.contains("energy: 3"));
    }

    // -- Metric routing --

    #[test]
    fn test_route_metric() {
        let cfg = default_config();
        let value = vec!["resting_hr".to_string(), "52".to_string()];
        let result = route_field(
            "metric",
            &value,
            "resting_hr 52",
            SAMPLE,
            &cfg,
            &empty_modules(),
        )
        .unwrap();
        assert!(result.contains("resting_hr: 52"));
    }

    #[test]
    fn test_route_metric_missing_value() {
        let cfg = default_config();
        let value = vec!["resting_hr".to_string()];
        let result = route_field(
            "metric",
            &value,
            "resting_hr",
            SAMPLE,
            &cfg,
            &empty_modules(),
        );
        assert!(result.is_err());
    }

    // -- Input validation tests --

    #[test]
    fn test_reject_invalid_weight() {
        let cfg = default_config();
        let result = route_field(
            "weight",
            &["banana".into()],
            "banana",
            SAMPLE,
            &cfg,
            &empty_modules(),
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Invalid weight"));
    }

    #[test]
    fn test_reject_negative_weight() {
        let cfg = default_config();
        let result = route_field(
            "weight",
            &["-5".into()],
            "-5",
            SAMPLE,
            &cfg,
            &empty_modules(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_reject_mood_out_of_range() {
        let cfg = default_config();
        let result = route_field(
            "mood",
            &["999".into()],
            "999",
            SAMPLE,
            &cfg,
            &empty_modules(),
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Expected 1-5"));
    }

    #[test]
    fn test_reject_mood_not_number() {
        let cfg = default_config();
        let result = route_field(
            "mood",
            &["great".into()],
            "great",
            SAMPLE,
            &cfg,
            &empty_modules(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_reject_energy_zero() {
        let cfg = default_config();
        let result = route_field("energy", &["0".into()], "0", SAMPLE, &cfg, &empty_modules());
        assert!(result.is_err());
    }

    #[test]
    fn test_reject_sleep_no_dash() {
        let cfg = default_config();
        let result = route_field(
            "sleep",
            &["10pm".into()],
            "10pm",
            SAMPLE,
            &cfg,
            &empty_modules(),
        );
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Expected start-end"));
    }

    #[test]
    fn test_sleep_quality_routes_correctly() {
        let cfg = default_config();
        let result = route_field(
            "sleep_quality",
            &["4".into()],
            "4",
            SAMPLE,
            &cfg,
            &empty_modules(),
        )
        .unwrap();
        assert!(result.contains("sleep_quality: 4"));
    }

    #[test]
    fn test_sleep_quality_validates() {
        let cfg = default_config();
        let result = route_field(
            "sleep_quality",
            &["9".into()],
            "9",
            SAMPLE,
            &cfg,
            &empty_modules(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_reject_metric_non_numeric() {
        let cfg = default_config();
        let value = vec!["resting_hr".into(), "banana".into()];
        let result = route_field(
            "metric",
            &value,
            "resting_hr banana",
            SAMPLE,
            &cfg,
            &empty_modules(),
        );
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Expected a number"));
    }

    #[test]
    fn test_accept_valid_inputs() {
        let cfg = default_config();
        // These should all succeed
        route_field(
            "weight",
            &["173.4".into()],
            "173.4",
            SAMPLE,
            &cfg,
            &empty_modules(),
        )
        .unwrap();
        route_field("mood", &["1".into()], "1", SAMPLE, &cfg, &empty_modules()).unwrap();
        route_field("mood", &["5".into()], "5", SAMPLE, &cfg, &empty_modules()).unwrap();
        route_field("energy", &["3".into()], "3", SAMPLE, &cfg, &empty_modules()).unwrap();
        route_field(
            "sleep",
            &["10pm-6am".into()],
            "10pm-6am",
            SAMPLE,
            &cfg,
            &empty_modules(),
        )
        .unwrap();
    }

    // -- Module routing tests using real Training module --

    #[test]
    fn test_route_lift_nested() {
        let config = test_config();
        let modules = crate::modules::build_registry(&config);
        let value = vec!["pullup".to_string(), "BWx8".to_string()];
        let result = route_field("lift", &value, "pullup BWx8", SAMPLE, &config, &modules).unwrap();
        assert!(result.contains("  pullup: BWx8"));
        // Existing lifts preserved
        assert!(result.contains("  squat: 185x5, 185x5"));
    }

    #[test]
    fn test_route_lift_missing_value() {
        let config = test_config();
        let modules = crate::modules::build_registry(&config);
        let value = vec!["pullup".to_string()];
        let result = route_field("lift", &value, "pullup", SAMPLE, &config, &modules);
        assert!(result.is_err());
    }

    #[test]
    fn test_route_unknown_field() {
        let cfg = default_config();
        let result = route_field(
            "banana",
            &["x".to_string()],
            "x",
            SAMPLE,
            &cfg,
            &empty_modules(),
        );
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Unknown field"));
    }

    // -- Weight unit in error messages --

    #[test]
    fn test_weight_error_includes_unit_lbs() {
        let cfg = default_config();
        let result = route_field(
            "weight",
            &["banana".into()],
            "banana",
            SAMPLE,
            &cfg,
            &empty_modules(),
        );
        let err = result.unwrap_err().to_string();
        assert!(err.contains("lbs"), "error should mention lbs: {err}");
    }

    #[test]
    fn test_weight_error_includes_unit_kg() {
        let mut cfg = default_config();
        cfg.weight_unit = crate::config::WeightUnit::Kg;
        let result = route_field(
            "weight",
            &["9999".into()],
            "9999",
            SAMPLE,
            &cfg,
            &empty_modules(),
        );
        let err = result.unwrap_err().to_string();
        assert!(err.contains("kg"), "error should mention kg: {err}");
    }

    // -- File creation from template --

    #[test]
    fn test_file_creation_from_template() {
        let dir = tempfile::TempDir::new().unwrap();
        let notes_dir = dir.path().to_path_buf();
        let config = test_config_with_dir(notes_dir.to_str().unwrap());
        let modules = crate::modules::build_registry(&config);

        let today = Local::now().format("%Y-%m-%d").to_string();
        let note_path = notes_dir.join(format!("{today}.md"));

        // Note should not exist yet
        assert!(!note_path.exists());

        execute("weight", &["173.4".to_string()], &config, &modules).unwrap();

        // Note should now exist
        assert!(note_path.exists());
        let content = std::fs::read_to_string(&note_path).unwrap();
        assert!(content.contains(&format!("date: {today}")));
        assert!(content.contains("weight: 173.4"));
    }

    // -- Atomic write --

    #[test]
    fn test_atomic_write_roundtrip() {
        let dir = tempfile::TempDir::new().unwrap();
        let notes_dir = dir.path().to_path_buf();
        let config = test_config_with_dir(notes_dir.to_str().unwrap());
        let modules = crate::modules::build_registry(&config);

        // First write creates the file
        execute("weight", &["173.4".to_string()], &config, &modules).unwrap();

        let today = Local::now().format("%Y-%m-%d").to_string();
        let note_path = notes_dir.join(format!("{today}.md"));

        // Second write updates atomically
        execute("mood", &["5".to_string()], &config, &modules).unwrap();

        let content = std::fs::read_to_string(&note_path).unwrap();
        assert!(content.contains("weight: 173.4"));
        assert!(content.contains("mood: 5"));
    }

    // -- Test helpers --

    fn test_config() -> Config {
        let dir = tempfile::TempDir::new().unwrap();
        // Leak the TempDir so it lives for the duration of the test
        let dir = Box::leak(Box::new(dir));
        test_config_with_dir(dir.path().to_str().unwrap())
    }

    fn test_config_with_dir(notes_dir: &str) -> Config {
        let toml_str = format!(
            r#"
notes_dir = '{notes_dir}'

[modules]
dashboard = true
training = true
trends = true
climbing = false
"#
        );
        // Replace backslashes for Windows paths (TOML uses \\ or single-quoted literals)
        let toml_str = toml_str.replace('\\', "/");
        toml::from_str(&toml_str).unwrap()
    }
}
