use serde::Serialize;

use crate::error::CliError;

pub fn print_json<T: Serialize>(value: &T, _json: bool) -> Result<(), CliError> {
    let s = serde_json::to_string_pretty(value)
        .map_err(|e| CliError::Serialization(e.to_string()))?;
    println!("{s}");
    Ok(())
}
