use std::collections::HashMap;

pub fn parse_prom(text: &str) -> HashMap<String, f64> {
    let mut out = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let key;
        let remaining;
        if let Some(idx) = line.find('{') {
            key = &line[..idx];
            if let Some(r_idx) = line.rfind('}') {
                remaining = &line[r_idx + 1..];
            } else {
                remaining = &line[idx..];
            }
        } else {
            let mut parts = line.split_whitespace();
            key = parts.next().unwrap_or("");
            remaining = if key.is_empty() {
                ""
            } else {
                &line[key.len()..]
            };
        }

        let key = key.trim();
        if key.is_empty() {
            continue;
        }

        let mut parts = remaining.split_whitespace();
        if let Some(val_str) = parts.next() {
            if let Ok(val) = val_str.parse::<f64>() {
                if !val.is_nan() {
                    out.insert(key.to_string(), val);
                }
            }
        }
    }
    out
}
