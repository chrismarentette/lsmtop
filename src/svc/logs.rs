//! M7.3: log sources + engine-line styling. Port of
//! (_LOG_BASE / _LOG_SEMANTIC / colorize_engine_line) and LogScreen's
//! journalctl command (:732). applies rich `stylize` per pattern in
//! list order and LATER matches OVERWRITE — the faithful port reproduces
//! that overlay (including the quirk that the final bare-number rule repaints
//! digits inside timestamps/statuses). Explicit patterns only: arbitrary log
//! content can never inject styles or crash the viewer.
use ratatui::style::{Color, Modifier, Style};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogStyle {
    Default,
    Grey58,
    Grey42,
    Blue,
    Magenta,
    BoldCyan,
    Green,
    Yellow,
    Red,
    BrightCyan,
    DodgerBlue3,
    BoldGreen,
    Cyan,
    Dim,
    Bold,
    Violet,
}

impl LogStyle {
    pub fn to_rate(self) -> Style {
        let rgb = |r, g, b| Style::default().fg(Color::Rgb(r, g, b));
        match self {
            // rich named colors approximated on the 24-bit axis
            LogStyle::Default => Style::default(),
            LogStyle::Grey58 => rgb(148, 148, 148),
            LogStyle::Grey42 => rgb(97, 97, 97),
            LogStyle::Blue => rgb(0, 0, 255),
            LogStyle::Magenta => rgb(255, 0, 255),
            LogStyle::BoldCyan => Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
            LogStyle::Green => rgb(0, 255, 0),
            LogStyle::Yellow => rgb(255, 255, 0),
            LogStyle::Red => rgb(255, 0, 0),
            LogStyle::BrightCyan => rgb(0, 255, 255),
            LogStyle::DodgerBlue3 => rgb(0, 148, 255),
            LogStyle::BoldGreen => Style::default()
                .fg(Color::Rgb(0, 255, 0))
                .add_modifier(Modifier::BOLD),
            LogStyle::Cyan => rgb(0, 255, 255),
            LogStyle::Dim => Style::default().add_modifier(Modifier::DIM),
            LogStyle::Bold => Style::default().add_modifier(Modifier::BOLD),
            LogStyle::Violet => rgb(238, 130, 238),
        }
    }
}

/// // journalctl argv (-n 300 --no-pager -f, --user scope)
pub fn journal_cmd(unit: &str, scope: &str) -> Vec<String> {
    let mut cmd = vec!["journalctl".to_string()];
    if scope == "user" {
        cmd.push("--user".into());
    }
    cmd.extend(
        ["-u", unit, "-n", "300", "--no-pager", "-f"]
            .map(String::from),
    );
    cmd
}

/// Last n lines of a text blob [-n:]` semantics).
pub fn tail_lines(text: &str, n: usize) -> Vec<String> {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].iter().map(|s| s.to_string()).collect()
}

fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

struct Overlay {
    st: Vec<Option<LogStyle>>,
}

impl Overlay {
    fn new(len: usize) -> Self {
        Self { st: vec![None; len] }
    }
    fn paint(&mut self, a: usize, b: usize, s: LogStyle) {
        for i in a..b.min(self.st.len()) {
            self.st[i] = Some(s);
        }
    }
}

/// digit run at i (commas allowed inside), returns inclusive end (exclusive)
fn scan_digits(b: &[u8], i: usize) -> Option<usize> {
    if !b[i].is_ascii_digit() {
        return None;
    }
    let mut j = i + 1;
    while j < b.len() && (b[j].is_ascii_digit() || (b[j] == b',' && j + 1 < b.len() && b[j + 1].is_ascii_digit())) {
        j += 1;
    }
    Some(j)
}

/// \b\d[\d,]*(\.\d+)?\b — returns exclusive end after optional .digits
fn bare_number(b: &[u8], i: usize) -> Option<usize> {
    if i > 0 && is_word(b[i - 1]) {
        return None;
    }
    let mut j = scan_digits(b, i)?;
    if j < b.len() && b[j] == b'.' && j + 1 < b.len() && b[j + 1].is_ascii_digit() {
        j = scan_digits(b, j + 1)?;
    }
    if j < b.len() && is_word(b[j]) {
        return None;
    }
    Some(j)
}

const UNITS: [&str; 12] = [
    "tokens", "token", "threads", "thread", "ms", "tok", "kiB", "MiB", "GiB", "MB", "KB", "%",
];

fn find_lit(b: &[u8], pat: &[u8], from: usize) -> Option<usize> {
    if pat.is_empty() || from >= b.len() {
        return None;
    }
    (from..=b.len().checked_sub(pat.len())?).find(|&k| &b[k..k + pat.len()] == pat)
}

pub fn engine_line_spans(line: &str) -> Vec<(usize, usize, LogStyle)> {
    let b = line.as_bytes();
    let mut o = Overlay::new(b.len());

    // ---- BASE layer, list order (:606-621) ----
    // 1. ISO timestamp (+optional ,.frac)
    let mut i = 0;
    while i < b.len() {
        if let Some(end) = iso_ts_at(b, i) {
            o.paint(i, end, LogStyle::Grey58);
            i = end;
        } else {
            i += 1;
        }
    }
    // 2. journald prefix llama-server[-router][PID]:
    let mut i = 0;
    while let Some(p) = find_lit(b, b"llama-server", i) {
        let mut j = p + 12;
        if b.len() >= j + 7 && &b[j..j + 7] == b"-router" {
            j += 7;
        }
        if j < b.len() && b[j] == b'[' {
            if let Some(de) = scan_digits(b, j + 1) {
                if de + 1 < b.len() && b[de] == b']' && b[de + 1] == b':' {
                    o.paint(p, de + 2, LogStyle::Grey42);
                    i = de + 2;
                    continue;
                }
            }
        }
        i = p + 1;
    }
    // 3. [elapsed.secs]
    let mut i = 0;
    while i < b.len() {
        if b.get(i) == Some(&b'[') {
            if let Some(d1) = scan_digits(b, i + 1) {
                if d1 < b.len() && b[d1] == b'.' {
                    if let Some(d2) = scan_digits(b, d1 + 1) {
                        if d2 < b.len() && b[d2] == b']' {
                            o.paint(i, d2 + 1, LogStyle::Grey58);
                            i = d2 + 1;
                            continue;
                        }
                    }
                }
            }
        }
        i += 1;
    }
    // 4. IP(:port) / version numbers
    let mut i = 0;
    while i < b.len() {
        if let Some((end, groups)) = dotted_number(b, i) {
            let ip_like = groups >= 4;
            let version = groups == 3
                && (i == 0 || matches!(b[i - 1], b' ' | b'=' | b'('));
            let port_end = if end < b.len() && b[end] == b':' {
                scan_digits(b, end + 1).unwrap_or(end)
            } else {
                end
            };
            if ip_like || version {
                o.paint(i, port_end, LogStyle::Blue);
                i = port_end;
                continue;
            }
        }
        i += 1;
    }
    // 5. "quoted strings" (<=140 chars, no newline)
    let mut i = 0;
    while let Some(q) = find_lit(b, b"\"", i) {
        let mut k = q + 1;
        let mut close = None;
        while k < b.len() && b[k] != b'"' && b[k] != b'\n' {
            if k - q > 140 {
                break;
            }
            k += 1;
        }
        if k < b.len() && b[k] == b'"' && k - q <= 141 {
            close = Some(k + 1);
        }
        match close {
            Some(end) => {
                o.paint(q, end, LogStyle::Magenta);
                i = end;
            }
            None => i = q + 1,
        }
    }
    // 6. HTTP verbs followed by a space
    for verb in [b"GET".as_slice(), b"POST", b"PUT", b"DELETE"] {
        let mut i = 0;
        while let Some(p) = find_lit(b, verb, i) {
            let ok_start = p == 0 || !is_word(b[p - 1]);
            let ok_end = p + verb.len() < b.len()
                && b[p + verb.len()] == b' '
                && !is_word(b[p + verb.len()]);
            if ok_start && ok_end {
                o.paint(p, p + verb.len(), LogStyle::BoldCyan);
            }
            i = p + verb.len();
        }
    }
    // 7. HTTP status 2xx/4xx/5xx before ' NN.N ms'
    let mut i = 0;
    while i + 3 <= b.len() {
        if b[i].is_ascii_digit()
            && (i == 0 || !is_word(b[i - 1]))
            && b[i + 1].is_ascii_digit()
            && b[i + 2].is_ascii_digit()
            && i + 3 >= b.len()
            && i + 3 < b.len() && !is_word(b[i + 3])
        {
            let after = i + 3;
            let mut k = after;
            while k < b.len() && (b[k] == b' ' || b[k] == b'\t') {
                k += 1;
            }
            let ds = k;
            while k < b.len() && (b[k].is_ascii_digit() || b[k] == b'.') {
                k += 1;
            }
            let has_num = k > ds && k - ds > 1 || (k > ds && b[ds].is_ascii_digit());
            let mut m = k;
            while m < b.len() && (b[m] == b' ' || b[m] == b'\t') {
                m += 1;
            }
            if has_num && m + 2 <= b.len() && &b[m..(m + 2).min(b.len())] == b"ms" && (m + 2 >= b.len() || !is_word(b[m + 2]))
            {
                let st = match b[i] {
                    b'2' => LogStyle::Green,
                    b'4' => LogStyle::Yellow,
                    b'5' => LogStyle::Red,
                    _ => LogStyle::Default,
                };
                if st != LogStyle::Default {
                    o.paint(i, i + 3, st);
                }
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    // 8. number+unit  \b\d[\d,]*(\.\d+)?\s*(ms|tokens?|tok|kiB|MiB|GiB|MB|KB|threads?|%)\b
    let mut i = 0;
    while i < b.len() {
        if let Some(num_end) = bare_number(b, i) {
            let mut k = num_end;
            while k < b.len() && (b[k] == b' ' || b[k] == b'\t') {
                k += 1;
            }
            if let Some(u) = UNITS.iter().find(|u| {
                let ub = u.as_bytes();
                k + ub.len() <= b.len()
                    && &b[k..k + ub.len()] == ub
                    && (k + ub.len() >= b.len() || !is_word(b[k + ub.len()]))
            }) {
                o.paint(i, k + u.len(), LogStyle::BrightCyan);
                i = k + u.len();
                continue;
            }
        }
        i += 1;
    }
    // 9. bare numbers (LAST base rule — repaints digits everywhere)
    let mut i = 0;
    while i < b.len() {
        if let Some(end) = bare_number(b, i) {
            o.paint(i, end, LogStyle::DodgerBlue3);
            i = end;
        } else {
            i += 1;
        }
    }

    // ---- SEMANTIC layer, wins on overlaps (:623-634) ----
    // 1. N tokens per second (bold green, number included)
    let mut i = 0;
    while let Some(p) = find_lit(b, b"tokens per second", i) {
        let mut k = p;
        while k > 0 && (b[k - 1] == b' ' || b[k - 1] == b'\t') {
            k -= 1;
        }
        if k > 0 && b[k - 1].is_ascii_digit() {
            let mut s = k - 1;
            while s > 0 && (b[s - 1].is_ascii_digit() || b[s - 1] == b'.' || b[s - 1] == b',') {
                s -= 1;
            }
            // pattern has NO leading \b — match wherever digits end
            if b[s].is_ascii_digit() {
                o.paint(s, p + 17, LogStyle::BoldGreen);
            }
        }
        i = p + 17;
    }
    // 2. N.N ms per token
    let mut i = 0;
    while let Some(p) = find_lit(b, b"ms per token", i) {
        let mut k = p;
        while k > 0 && b[k - 1] == b' ' {
            k -= 1;
        }
        if k > 0 && b[k - 1].is_ascii_digit() {
            let mut s = k - 1;
            let mut had_dot = false;
            while s > 0 && (b[s - 1].is_ascii_digit() || (b[s - 1] == b'.' && !had_dot && {
                had_dot = true;
                true
            })) {
                s -= 1;
            }
            o.paint(s, p + 12, LogStyle::Green);
        }
        i = p + 12;
    }
    // 3. draft acceptance = X(.Y) [(...)]
    let mut i = 0;
    while let Some(p) = find_lit(b, b"draft acceptance = ", i) {
        let mut end = p + 19;
        if end < b.len() && b[end].is_ascii_digit() {
            end = scan_digits(b, end).unwrap_or(end);
            if end + 1 < b.len() && b[end] == b'.' && b[end + 1].is_ascii_digit() {
                end = scan_digits(b, end + 1).unwrap_or(end);
            }
            if end < b.len() && b[end] == b' ' && b[end + 1] == b'(' {
                if let Some(close) = find_lit(b, b")", end + 1) {
                    end = close + 1;
                }
            }
        }
        o.paint(p, end, LogStyle::Cyan);
        i = end;
    }
    // 4. error / exception / failed — red
    let mut i = 0;
    while let Some(p) = find_lit_ci_word(b, b"error", i) {
        o.paint(p, p + 5, LogStyle::Red);
        i = p + 5;
    }
    for lit in [b"exception".as_slice(), b"failed"] {
        let mut i = 0;
        while let Some(p) = find_lit(b, lit, i) {
            o.paint(p, p + lit.len(), LogStyle::Red);
            i = p + lit.len();
        }
    }
    // 5. abort(ed) / warn(ing) / "W |" — yellow
    for lit in [b"aborted".as_slice(), b"abort", b"warning", b"warn"] {
        let mut i = 0;
        while let Some(p) = find_lit_word(b, lit, i) {
            o.paint(p, p + lit.len(), LogStyle::Yellow);
            i = p + lit.len();
        }
    }
    let mut i = 0;
    while let Some(p) = find_lit(b, b"W |", i) {
        if p == 0 || !is_word(b[p - 1]) {
            o.paint(p, p + 3, LogStyle::Yellow);
        }
        i = p + 3;
    }
    // 6. "| task N |" / "| id N |" — dim
    for lit in [b"| task ".as_slice(), b"| id "] {
        let mut i = 0;
        while let Some(p) = find_lit(b, lit, i) {
            let mut end = p + lit.len();
            if let Some(de) = scan_digits(b, end) {
                if de + 1 < b.len() && b[de] == b' ' && b[de + 1] == b'|' {
                    end = de + 2;
                }
            }
            o.paint(p, end, LogStyle::Dim);
            i = end;
        }
    }
    // 7. timings words — bold
    for lit in [b"prompt eval time".as_slice(), b"eval time", b"total time", b"timings"] {
        let mut i = 0;
        while let Some(p) = find_lit(b, lit, i) {
            o.paint(p, p + lit.len(), LogStyle::Bold);
            i = p + lit.len();
        }
    }
    // 8. srv hooks / main: — violet
    for lit in [
        b"srv  update_".as_slice(),
        b"srv  load_model",
        b"srv  init",
        b"main:",
    ] {
        let mut i = 0;
        while let Some(p) = find_lit(b, lit, i) {
            o.paint(p, p + lit.len(), LogStyle::Violet);
            i = p + lit.len();
        }
    }

    // merge runs
    let mut out: Vec<(usize, usize, LogStyle)> = Vec::new();
    let mut idx = 0;
    while idx < o.st.len() {
        if let Some(s) = o.st[idx] {
            let start = idx;
            while idx < o.st.len() && o.st[idx] == Some(s) {
                idx += 1;
            }
            out.push((start, idx, s));
        } else {
            idx += 1;
        }
    }
    out
}

fn iso_ts_at(b: &[u8], i: usize) -> Option<usize> {
    let d = |k: usize, n: usize| -> Option<usize> {
        if k + n > b.len() {
            return None;
        }
        (0..n).all(|x| b[k + x].is_ascii_digit()).then_some(k + n)
    };
    let mut j = d(i, 4)?;
    if b.get(j) != Some(&b'-') {
        return None;
    }
    j = d(j + 1, 2)?;
    if b.get(j) != Some(&b'-') {
        return None;
    }
    j = d(j + 1, 2)?;
    if !matches!(b.get(j), Some(b'T') | Some(b' ')) {
        return None;
    }
    j = d(j + 1, 2)?;
    if b.get(j) != Some(&b':') {
        return None;
    }
    j = d(j + 1, 2)?;
    if b.get(j) != Some(&b':') {
        return None;
    }
    j = d(j + 1, 2)?;
    if j < b.len() && (b[j] == b'.' || b[j] == b',') && j + 1 < b.len() && b[j + 1].is_ascii_digit() {
        j = scan_digits(b, j + 1)?;
    }
    Some(j)
}

/// digits(.digits){2,3} — returns (end, group_count)
fn dotted_number(b: &[u8], i: usize) -> Option<(usize, usize)> {
    if !b[i].is_ascii_digit() || (i > 0 && (is_word(b[i - 1]) || b[i - 1] == b'.')) {
        return None;
    }
    let mut j = scan_digits(b, i)?;
    let mut groups = 1;
    while j + 1 < b.len() && b[j] == b'.' && b[j + 1].is_ascii_digit() && groups < 4 {
        j = scan_digits(b, j + 1)?;
        groups += 1;
    }
    if groups >= 3 && (j >= b.len() || b[j] != b'.') {
        Some((j, groups))
    } else {
        None
    }
}

fn find_lit_word(b: &[u8], pat: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while let Some(p) = find_lit(b, pat, i) {
        let ok = (p == 0 || !is_word(b[p - 1]))
            && (p + pat.len() >= b.len() || !is_word(b[p + pat.len()]));
        if ok {
            return Some(p);
        }
        i = p + 1;
    }
    None
}

fn find_lit_ci_word(b: &[u8], pat: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while i + pat.len() <= b.len() {
        if (i == 0 || !is_word(b[i - 1]))
            && (0..pat.len()).all(|k| b[i + k].eq_ignore_ascii_case(&pat[k]))
            && (i + pat.len() >= b.len() || !is_word(b[i + pat.len()]))
        {
            return Some(i);
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style_at(spans: &[(usize, usize, LogStyle)], pos: usize) -> Option<LogStyle> {
        spans.iter().find(|(a, e, _)| *a <= pos && pos < *e).map(|(_, _, s)| *s)
    }

    #[test]
    fn journal_cmd_scopes_and_flags() {
        let c = journal_cmd("llama-server", "user");
        assert_eq!(c[0], "journalctl");
        assert_eq!(c[1], "--user"); // only for user scope
        assert!(c.windows(2).any(|w| w[0] == "-u" && w[1] == "llama-server"));
        assert!(c.windows(2).any(|w| w[0] == "-n" && w[1] == "300"));
        assert!(c.contains(&"--no-pager".to_string()) && c.contains(&"-f".to_string()));
        let sys = journal_cmd("ssh", "system");
        assert!(!sys.contains(&"--user".to_string()));
    }

    #[test]
    fn tail_lines_is_python_negative_slice() {
        let v = tail_lines("a\nb\nc\n", 2);
        assert_eq!(v, vec!["b", "c"]);
        assert_eq!(tail_lines("a\nb", 10).len(), 2); // n > lines: all, no panic
        assert!(tail_lines("", 5).is_empty());
        assert!(tail_lines("a\nb", 0).is_empty());
    }

    #[test]
    fn to_rate_maps_distinct_inks() {
        assert_eq!(LogStyle::Red.to_rate().fg, Some(Color::Rgb(255, 0, 0)));
        assert!(LogStyle::BoldCyan.to_rate().add_modifier.contains(Modifier::BOLD));
        assert!(LogStyle::Dim.to_rate().add_modifier.contains(Modifier::DIM));
        assert_eq!(LogStyle::Default.to_rate(), Style::default());
    }

    #[test]
    fn spans_are_disjoint_and_in_bounds() {
        let line = "2026-11-07T21:24:16.123Z llama-server-router[4242]: 192.0.2.5:53122 - \"GET /health HTTP/1.1\" 200 in 1.2 ms";
        let spans = engine_line_spans(line);
        let mut prev_end = 0;
        for (a, e, _) in &spans {
            assert!(*a < *e && *e <= line.len(), "bad span {a}..{e}");
            assert!(*a >= prev_end, "overlapping/unsorted spans");
            prev_end = *e;
        }
    }


    #[test]
    fn engine_line_law_full_run_lock() {
        // EXACT lock of the ported oracle on a canonical journald+HTTP line,
        // verified 2026-11-07. The documented QUIRK is part of the law: the
        // final bare-number rule repaints EVERY digit token — inside the
        // ISO stamp, the PID, the IP:port, even the 200 status — DodgerBlue3;
        // non-digit seams keep their layer colours.
        let line = "2026-11-07T21:24:16.123Z llama-server-router[4242]: 192.0.2.5:53122 - \"GET /health HTTP/1.1\" 200 in 1.2 ms";
        use LogStyle::*;
        assert_eq!(engine_line_spans(line), vec![
            (0, 4, DodgerBlue3), (4, 5, Grey58), (5, 7, DodgerBlue3),
            (7, 14, Grey58), (14, 16, DodgerBlue3), (16, 23, Grey58),
            (25, 45, Grey42), (45, 49, DodgerBlue3), (49, 51, Grey42),
            (52, 57, DodgerBlue3), (57, 58, Blue), (58, 61, DodgerBlue3),
            (61, 62, Blue), (62, 67, DodgerBlue3),
            (70, 71, Magenta), (71, 74, BoldCyan), (74, 88, Magenta),
            (88, 91, DodgerBlue3), (91, 92, Magenta),
            (93, 96, DodgerBlue3), (100, 103, DodgerBlue3), (103, 106, BrightCyan),
        ]);
    }

    #[test]
    fn measured_lines_light_up() {
        let l = "slot print_details:  42.31 tokens per second";
        let s = engine_line_spans(l);
        let t = l.find("42.31").unwrap();
        let e = l.find("second").unwrap();
        assert!(matches!(style_at(&s, t), Some(LogStyle::BoldGreen)), "{s:?}");
        assert!(matches!(style_at(&s, e), Some(LogStyle::BrightCyan) | Some(LogStyle::BoldGreen)));
    }

}

    #[test]
    fn junk_and_empty_never_crash_and_never_inject() {
        for junk in ["", "{", "\"", "\u{1}\u{2}", "GET GETGET 999.999.999.999 [x]", "\u{0}error\u{0}5"] {
            let s = engine_line_spans(junk);
            for (a, e, _) in &s {
                assert!(*a < *e && *e <= junk.len());
            }
        }
        // NO arbitrary text is Bold/Red: styles come only from the law
        let s = engine_line_spans("plain english sentence nothing special");
        assert!(s.iter().all(|(_, _, st)| matches!(st, LogStyle::Grey58 | LogStyle::Grey42)), "{s:?}");
    }
