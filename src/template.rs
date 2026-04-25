use crate::config::Config;

const DAILY_NOTE: &str = include_str!("../templates/daily-note.md");

/// Render the daily-note template for `date`, substituting placeholders
/// (date and weight unit) using values from `config`.
pub fn render_daily_note(date: &str, config: &Config) -> String {
    DAILY_NOTE
        .replace("DATE_PLACEHOLDER", date)
        .replace("WEIGHT_UNIT_PLACEHOLDER", &config.weight_unit.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn config_with_unit(unit: &str) -> Config {
        toml::from_str(&format!(
            "notes_dir = '/tmp/test'\nweight_unit = '{unit}'\n"
        ))
        .expect("config parses")
    }

    #[test]
    fn renders_date_placeholder() {
        let config = config_with_unit("lbs");
        let out = render_daily_note("2026-04-17", &config);
        assert!(
            out.contains("date: 2026-04-17"),
            "expected rendered date, got: {out}"
        );
        assert!(
            !out.contains("DATE_PLACEHOLDER"),
            "DATE_PLACEHOLDER should be replaced"
        );
    }

    #[test]
    fn renders_weight_unit_lbs() {
        let config = config_with_unit("lbs");
        let out = render_daily_note("2026-04-17", &config);
        assert!(
            out.contains("weight:                   # lbs"),
            "expected `# lbs` weight comment, got: {out}"
        );
    }

    #[test]
    fn renders_weight_unit_kg() {
        let config = config_with_unit("kg");
        let out = render_daily_note("2026-04-17", &config);
        assert!(
            out.contains("weight:                   # kg"),
            "expected `# kg` weight comment, got: {out}"
        );
        assert!(
            !out.contains("# lbs"),
            "lbs comment should not appear when unit is kg, got: {out}"
        );
    }
}
