#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fill {
    Bar,
    Area,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chars {
    Braille(Option<Fill>),
    Block(Option<Fill>),
    Line(Option<Fill>),
}

pub fn eighth_block(value: f64) -> char {
    let bars = ['\u{2581}', '\u{2582}', '\u{2583}', '\u{2584}', '\u{2585}', '\u{2586}', '\u{2587}', '\u{2588}'];
    let v = value.clamp(0.0, 1.0);
    if v == 0.0 {
        return bars[0];
    }
    let mut idx = (v * 8.0).ceil() as usize;
    if idx == 0 {
        idx = 1;
    }
    bars[(idx - 1).min(7)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_eighth_block_map() {
        let cases = [
            (0.0, '\u{2581}'),
            (0.125, '\u{2581}'),
            (0.5, '\u{2584}'),
            (0.99, '\u{2588}'),
            (1.0, '\u{2588}'),
        ];
        for (v, expected) in cases {
            assert_eq!(eighth_block(v), expected, "failed for {}", v);
        }
    }
}
