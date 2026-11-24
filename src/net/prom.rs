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

#[cfg(test)]
mod tests {
    use super::parse_prom;

    #[test]
    fn plain_and_labelled_lines_key_by_base_name() {
        let m = parse_prom(
            "# HELP x\n# TYPE x gauge\nllamacpp:tokens_predicted_total{model=\"m\",instance=\"i\"} 42.5\nbare_metric 7\n",
        );
        assert_eq!(m.get("llamacpp:tokens_predicted_total"), Some(&42.5));
        assert_eq!(m.get("bare_metric"), Some(&7.0));
        assert!(!m.contains_key("#")); // comments never enter
    }

    #[test]
    fn junk_is_skipped_never_poisoned() {
        let m = parse_prom("weird{unbalanced 3\nnanish NaN\nnovalue\n\n  \nok_after_junk 1.5\n");
        assert!(!m.contains_key("weird"));
        assert!(!m.contains_key("nanish")); // NaN poisons rates — must not land
        assert_eq!(m.get("ok_after_junk"), Some(&1.5));
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn last_value_wins_like_prometheus_text() {
        let m = parse_prom("dup 1\ndup 2\n");
        assert_eq!(m.get("dup"), Some(&2.0));
    }
}
