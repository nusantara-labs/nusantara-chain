use std::collections::HashMap;
use std::fs;

fn main() {
    println!("cargo:rerun-if-changed=config.toml");
    let content = fs::read_to_string("config.toml").expect("config.toml not found");
    let values = parse_toml_flat(&content);
    for (key, value) in &values {
        let env_key = format!("NUSA_{}", key.to_uppercase());
        println!("cargo:rustc-env={env_key}={value}");
    }
}

/// Simple flat TOML parser: returns section_key pairs
/// e.g., [gossip] push_interval_ms = 100 -> "GOSSIP_PUSH_INTERVAL_MS" = "100"
fn parse_toml_flat(content: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let mut section = String::new();
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            section = line[1..line.len() - 1].trim().to_string();
        } else if let Some((k, v)) = line.split_once('=') {
            let key = format!("{}_{}", section, k.trim());
            let raw = v.trim();
            let val = if raw.starts_with('"') && raw.ends_with('"') {
                // String value: strip quotes, keep underscores
                raw[1..raw.len() - 1].to_string()
            } else {
                // Numeric value: strip underscores
                raw.replace('_', "")
            };
            map.insert(key, val);
        }
    }
    map
}
