//! M7.3: log-source commands, tail semantics, and engine-line styling.
use lsmtop::svc::logs::{engine_line_spans, journal_cmd, tail_lines, LogStyle};

fn style_at(spans: &[(usize, usize, LogStyle)], idx: usize) -> Option<LogStyle> {
    spans
        .iter()
        .find(|(a, b, _)| *a <= idx && idx < *b)
        .map(|(_, _, s)| *s)
}

#[test]
fn journal_cmd_exact_parity() {
    // journalctl [-u unit -n 300 --no-pager -f] with --user
    assert_eq!(
        journal_cmd("llama-server", "system"),
        ["journalctl", "-u", "llama-server", "-n", "300", "--no-pager", "-f"]
    );
    let u = journal_cmd("llama", "user");
    assert_eq!(&u[..3], ["journalctl", "--user", "-u"]);
    assert_eq!(&u[3..], ["llama", "-n", "300", "--no-pager", "-f"]);
}

#[test]
fn tail_lines_semantics() {
    assert_eq!(tail_lines("a\nb\nc", 2), ["b", "c"]);
    assert_eq!(tail_lines("x", 300), ["x"]);
    assert_eq!(tail_lines("", 300), Vec::<String>::new());
    assert_eq!(tail_lines("a\nb\n", 300), ["a", "b"]); // trailing blank dropped like splitlines
}

#[test]
fn semantic_layer_wins_over_base() {
    let line = "print_timing: 12.34 tokens per second";
    let spans = engine_line_spans(line);
    let i = line.find("tokens per second").unwrap();
    let j = line.find("12.34").unwrap();
    // the WHOLE phrase including the number is bold green (semantic beat
    // the base bright_cyan/bare-number styles)
    assert_eq!(style_at(&spans, j), Some(LogStyle::BoldGreen));
    assert_eq!(style_at(&spans, i + 3), Some(LogStyle::BoldGreen));
}

#[test]
fn error_abort_and_warn_semantics() {
    let s = engine_line_spans("ERROR failed to load model");
    let i = s.len(); // len of spans vector is irrelevant; index by line
    let _ = i;
    let line = "ERROR failed to load model";
    assert_eq!(style_at(&s, line.find("ERROR").unwrap()), Some(LogStyle::Red));
    assert_eq!(style_at(&s, line.find("failed").unwrap()), Some(LogStyle::Red));
    let w = engine_line_spans("warning: degraded abort path");
    let wl = "warning: degraded abort path";
    assert_eq!(style_at(&w, wl.find("warning").unwrap()), Some(LogStyle::Yellow));
    assert_eq!(style_at(&w, wl.find("abort").unwrap()), Some(LogStyle::Yellow));
}

#[test]
fn base_layer_styles() {
    // FAITHFUL-PORT ORDER (quirk included): _LOG_BASE applies rules in list
    // order and rich stylize OVERWRITES, so the final bare-number rule
    // repaints every digit — status green and unit-number cyan survive only
    // where the bare pattern does not reach (the unit WORD, separators).
    let line = "GET /completion 200 in 12.3 ms \"some req\" main: init done 192.168.1.10:8080";
    let spans = engine_line_spans(line);
    assert_eq!(style_at(&spans, line.find("GET").unwrap()), Some(LogStyle::BoldCyan));
    assert_eq!(
        style_at(&spans, line.find("200").unwrap()),
        Some(LogStyle::DodgerBlue3),
        "bare-number rule (last) overwrites status green"
    );
    let q = line.find("some req").unwrap();
    assert_eq!(style_at(&spans, q), Some(LogStyle::Magenta));
    assert_eq!(style_at(&spans, line.find("main:").unwrap()), Some(LogStyle::Violet));
    assert_eq!(
        style_at(&spans, line.find("192.168.1.10").unwrap()),
        Some(LogStyle::DodgerBlue3), // faithful quirk: bare rule repaints IP digits
    );
    assert_eq!(
        style_at(&spans, line.find("ms").unwrap()),
        Some(LogStyle::BrightCyan),
        "unit word keeps bright_cyan (digits get repainted blue)"
    );
}

#[test]
fn markup_cannot_inject_and_no_panics_on_junk() {
    // doctrine: explicit patterns only, never markup parsing —
    // arbitrary content cannot style itself and must never crash.
    for junk in [
        "[red]pwn[/red]", "\u{1}[![]\u{0}", "\"unclosed quote", "…".repeat(200).as_str(),
        "", "///", "%d", "192.168", "00:00:00",
    ] {
        let spans = engine_line_spans(junk);
        for (a, b, _) in &spans {
            assert!(a < b && *b <= junk.len(), "span out of bounds on {junk:?}");
        }
    }
    // a markup-looking word 'red' alone is not an error style
    let s = engine_line_spans("[red]pwn[/red]");
    assert!(!s.iter().any(|(_, _, st)| *st == LogStyle::Red));
}

#[test]
fn journald_prefix_and_iso_timestamps() {
    let line = "Nov 03 21:00:00 host llama-server-router[4242]: 2026-11-03T21:00:00.123 srv  load_model ok";
    let spans = engine_line_spans(line);
    assert_eq!(
        style_at(&spans, line.find("llama-server-router[4242]:").unwrap() + 5),
        Some(LogStyle::Grey42),
        "letters keep grey42; digits repainted by bare rule"
    );
    let ts = line.find("2026-11-03T21:00:00").unwrap();
    assert_eq!(style_at(&spans, ts), Some(LogStyle::DodgerBlue3)); // faithful quirk
    assert_eq!(style_at(&spans, ts + 4), Some(LogStyle::Grey58)); // '-' keeps grey58
    assert_eq!(
        style_at(&spans, line.find("srv  load_model").unwrap() + 4),
        Some(LogStyle::Violet)
    );
}
