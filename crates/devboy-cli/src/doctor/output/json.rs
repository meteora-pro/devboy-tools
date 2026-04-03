use anyhow::Result;
use serde::Serialize;

pub fn print_json_report<T: Serialize>(value: &T) -> Result<()> {
    println!("{}", render_json_report(value)?);
    Ok(())
}

fn render_json_report<T: Serialize>(value: &T) -> Result<String> {
    Ok(serde_json::to_string_pretty(value)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn render_json_report_pretty_formats_values() {
        let rendered = render_json_report(&json!({"ok": true, "count": 2})).unwrap();
        assert!(rendered.contains("\n"));
        assert!(rendered.contains("\"ok\": true"));
        assert!(rendered.contains("\"count\": 2"));
    }
}
