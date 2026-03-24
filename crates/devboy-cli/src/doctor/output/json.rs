use anyhow::Result;
use serde::Serialize;

pub fn print_json_report<T: Serialize>(value: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
