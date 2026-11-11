//! lsmtop — top-style live monitor for llama.cpp / llama-swap (Rust).
//! Metrics-first by doctrine: throughput, KV, cache, GPU, journal. Server
//! lifecycle (start/stop/HUP) and model downloads are separate tools with
//! separate UIs — one tool does one thing, and this one measures.
use std::collections::HashMap;
use std::env;
use std::io::{self, BufRead, Seek, SeekFrom};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode,
        MouseButton, MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Terminal,
};

use lsmtop::app::{Action, App, AppEvent, EngineMode, SampleData, Screen, SetKey};
use lsmtop::config::{tick_step_from, Config};
use lsmtop::net::collector;
use lsmtop::paint::glyphs::{Chars, Fill};
use lsmtop::paint::modes::Mode;
use lsmtop::paint::ramps::Ramp;
use lsmtop::sampling::Sampler;
use lsmtop::svc::logs::journal_cmd;
use lsmtop::svc::{systemctl_show, RealRunner, UnitInfo};
use lsmtop::ui::dashboard::{Part,
    dash_avail_h, graph_layout, graphs_min, graphs_rect, journal_pane_h, journal_pane_rect,
    metrics_strip, ring_stats, strip_layout, strip_text, DashRow, DashboardWidget, JournalPane,
    Legend, PaneSize, StripTok,
};
use lsmtop::ui::graph_widget::GraphOpts;
use lsmtop::ui::logs::{LogSource, LogState, LogsWidget};
use lsmtop::ui::opts::OptsState;
use lsmtop::ui::strip::{self, FootTok};
use lsmtop::ui::style;

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// monotonic-ish ms clock for the key-repeat gate (tests inject their own)
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn logs_dir() -> std::path::PathBuf {
    let home = env::var("HOME").unwrap_or_else(|_| ".".into());
    std::path::PathBuf::from(home).join(".lsmtop").join("logs")
}

fn graph_opts_from(o: &OptsState) -> GraphOpts {
    let fill = match o.value("fill").as_str() {
        "bar" | "vert" => Some(Fill::Bar),
        "area" | "horiz" => Some(Fill::Area),
        _ => None,
    };
    GraphOpts {
        squish: o.value("timebase") == "squish",
        chars: match o.value("chars").as_str() {
            "block" => Chars::Block(fill),
            "line" => Chars::Line(fill),
            _ => Chars::Braille(fill),
        },
        colors: Mode::from_name(&o.value("colors")),
        ramp: Ramp::from_name(&o.value("ramp")),
    }
}

// ───────────────────────── headless fixture mode ─────────────────────────
// REAL fixture JSON → Sampler → dashboard text on stdout. M9's paint-parity
// hook; no network, no raw mode, exit 0.

fn read_ndecoded(nt: &serde_json::Value) -> Option<&serde_json::Value> {
    // llama.cpp quirk (measured 2026-11-10): next_token serializes as a
    // single-element ARRAY; n_decoded is the running decode total.
    let one = nt.as_array().and_then(|a| a.first()).unwrap_or(nt);
    one.get("n_decoded")
}

fn headless_fixture(dir: &str) -> io::Result<()> {
    let models_str = std::fs::read_to_string(format!("{dir}/models.json")).unwrap_or_default();
    let slots_str = std::fs::read_to_string(format!("{dir}/slots.json")).unwrap_or_default();
    let metrics_str = std::fs::read_to_string(format!("{dir}/metrics.prom")).unwrap_or_default();
    let models: serde_json::Value =
        serde_json::from_str(&models_str).unwrap_or_else(|_| serde_json::json!({"data": []}));
    let slots: serde_json::Value =
        serde_json::from_str(&slots_str).unwrap_or_else(|_| serde_json::json!([]));
    let metrics = lsmtop::net::prom::parse_prom(&metrics_str);

    let mut lanes = HashMap::new();
    for (_m, port) in lsmtop::net::child_ports(&models) {
        lanes.insert(
            port.to_string(),
            lsmtop::net::LaneSample {
                slots: Some(slots.clone()),
                metrics: Some(metrics.clone()),
            },
        );
    }
    let sample = SampleData::Router(lsmtop::net::RouterSample {
        models: models.clone(),
        lanes,
    });

    let mut sampler = Sampler::new();
    sampler.ingest(1000.0, &sample);
    sampler.ingest(1001.0, &sample);
    sampler.gpu_busy = lsmtop::gpu::gpu_busy_percent().map(|v| v as f64);
    sampler.gpu_clk = lsmtop::gpu::gpu_clock_mhz();
    sampler.gpu_pwr = lsmtop::gpu::gpu_power_w();
    let (mem_used, mem_total) = lsmtop::gpu::sysinfo_mem();
    sampler.ram_percent = Some(if mem_total > 0.0 { mem_used / mem_total * 100.0 } else { 0.0 });

    let cfg = Config::load(None::<String>).map_err(|e| io::Error::other(e.to_string()))?;
    let opts_state = OptsState::from_config(&cfg);
    let gopts = graph_opts_from(&opts_state);

    let (rows_dash, _loaded_n, _total_n) = build_table_rows(Some(&models), &sampler.rows, true, None, &mut Vec::new(), sampler.focus.as_deref());
    let vis = [
        opts_state.value("gen_graph") == "on",
        opts_state.value("prompt_graph") == "on",
        opts_state.value("kv_graph") == "on",
        opts_state.value("gpu_graph") == "on",
    ];
    let sizes = {
        let mv = cfg.metrics();
        [
            PaneSize::from_mode(&mv.gen_size),
            PaneSize::from_mode(&mv.prompt_size),
            PaneSize::from_mode(&mv.kv_size),
            PaneSize::from_mode(&mv.gpu_size),
        ]
    };
    let strip_toks = metrics_strip();
    let stext = strip_text(&strip_toks);
    let backend = ratatui::backend::TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend)?;
    terminal.draw(|f| {
        let s = &mut sampler;
        let w = DashboardWidget {
            seam: None,
            stats: "fixture \u{2014} offline capture",
            hotkeys: &stext,
            hotkey_toks: &strip_toks,
            opts: gopts.clone(),
            tick: s.polls,
            tick_ms: 2000, // fixture: reference cadence
            gen: &s.gen,
            gen_leaky: &mut s.gen_leaky,
            gen_legend: legend("GEN t/s", avg_max(&s.gen)),
            prompt: &s.prompt,
            prompt_leaky: &mut s.prompt_leaky,
            prompt_legend: legend("PROMPT t/s", avg_max(&s.prompt)),
            kv: &s.kv,
            kv_legend: legend("KV\u{2248} %", Vec::new()),
            apu: &s.apu,
                apu_legend: legend(
                                        apu_title(),
                    // fixture offline: no live electrical number
                    Vec::new(),
                ),
            vis,
            sizes,
            graph_scroll: 0,
            show_lanes: true,
            journal: None,
            journal_h: 0,
            rows: rows_dash.clone(),
            scale: lsmtop::ui::dashboard::ScalePos::Right,
        };
        f.render_widget(w, f.area());
    })?;
    let buf = terminal.backend().buffer().clone();
    for y in 0..buf.area().height {
        let line: String = (0..buf.area().width)
            .map(|x| {
                buf.cell((x, y))
                    .map(|c| c.symbol().chars().next().unwrap_or(' '))
                    .unwrap_or(' ')
            })
            .collect();
        println!("{}", line.trim_end());
    }
    Ok(())
}

// ───────────────────────── runtime state ─────────────────────────

/// One target's whole view-state. Every collector feeds ONLY its own index
/// and the dashboard paints the ACTIVE one (target_ix, cycled with `t`):
/// one server's outage rails ITS curves, never another server's. The R8
/// lane-alive ghost memory lives here too — ghosts belong to the box (the
/// server) that saw the lane, not to the app.
struct TState {
    sampler: Sampler,
    last_models: Option<serde_json::Value>,
    /// R8 lane-alive: ids that have been loaded/swapping since start —
    /// they keep a ghost row after unload so the LANES box never resizes
    /// (maintainer 2026-11-10). Cleared only when a dead server empties the bar.
    lane_alive: Vec<String>,
    last_sample_err: Option<String>,
    /// what the endpoint ANSWERED as (payload-classified, never the
    /// config label) — the banner reads it for unit-less targets
    mode: EngineMode,
}

impl TState {
    fn new() -> Self {
        Self {
            sampler: Sampler::new(),
            last_models: None,
            lane_alive: Vec::new(),
            last_sample_err: None,
            mode: EngineMode::Unknown,
        }
    }
}

struct Rt {
    config: Config,
    config_path: std::path::PathBuf,
    stack: Vec<Screen>,
    target_ix: usize,
    /// one TState per config target, index-aligned with the collectors
    states: Vec<TState>,
    opts: OptsState,
    /// B2: graphs pane scroll offset when squeezed; the draw snapshots
    /// (window rect, max_scroll) so wheel events can scroll the pane.
    graph_scroll: u16,
    graphs_win: Option<(ratatui::layout::Rect, u16)>,
    /// B4: journal lower pane state (its own tail LogState + child)
    jpane: Option<LogState>,
    jpane_child: Option<std::process::Child>,
    /// stop-flags for owned-file pane followers (toggled on pane resize/close)
    jpane_flags: Vec<Arc<AtomicBool>>,
    jpane_mode: String,
    jpane_win: Option<ratatui::layout::Rect>,
    /// journal-truth running total for the TOTAL column (n_gen decode /
    /// n_tokens+pct prompt; cleared at release)
    jtot: Option<lsmtop::sampling::JournalTotal>,
    logs: Option<LogState>,
    log_child: Option<Child>,
    log_flags: Vec<Arc<AtomicBool>>,
    collector: Vec<(JoinHandle<()>, Arc<AtomicBool>)>,
    /// tier the journal was on before J switched it off (restore target)
    journal_last: String,
    /// lanes size before L switched it off: "on" | "all" (restore target)
    lanes_last: String,
    unit_infos: HashMap<usize, Result<UnitInfo, String>>,
    units_polled: Instant,
    setst: Option<SetFocus>,
    /// hold-to-repeat gate for the Settings tick [ + ] / [ - ] keys
    step_gate: Option<StepGate>,
    /// interactive rects recorded by the LAST render (mouse hit map)
    hit: Vec<(Rect, Hit)>,
    dirty: bool,
}

impl Rt {
    /// the ACTIVE target's view (never empty: init guarantees >= 1)
    fn ts(&self) -> &TState {
        &self.states[self.target_ix.min(self.states.len() - 1)]
    }
    fn ts_mut(&mut self) -> &mut TState {
        let i = self.target_ix.min(self.states.len() - 1);
        &mut self.states[i]
    }
    fn target(&self) -> serde_json::Value {
        self.config.targets().into_iter().nth(self.target_ix).unwrap_or_else(|| {
            serde_json::json!({"kind": "router", "name": "none", "base_url": "http://127.0.0.1:8085"})
        })
    }
    fn top(&self) -> Screen {
        *self.stack.last().unwrap_or(&Screen::Metrics)
    }
    /// R5: action-feedback toast eliminated by request — results live in
    /// the state the action changed (rows, chips, tick value). Call sites
    /// keep their intent readable; nothing renders this.
    fn say(&mut self, _msg: impl Into<String>) {}
}

/// collector::spawn's flag: `true` = STOP (its loop is `while !stop`).
/// The cadence comes from the config Tick rate — one scrape per tick.
fn spawn_collector(
    target: serde_json::Value,
    tx: Sender<AppEvent>,
    tick_ms: u32,
    ix: usize,
) -> (JoinHandle<()>, Arc<AtomicBool>) {
    let stop = Arc::new(AtomicBool::new(false));
    let handle = collector::spawn(target, tx, stop.clone(), tick_ms, ix);
    (handle, stop)
}

/// One collector PER config target; samples carry the index so the UI
/// routes each into ITS OWN TState (multi-server law, 2026-11-10).
fn spawn_all(rt: &mut Rt, tx: &Sender<AppEvent>) {
    let tick = rt.config.tick_ms();
    rt.collector = rt
        .config
        .targets()
        .into_iter()
        .enumerate()
        .map(|(ix, t)| spawn_collector(t, tx.clone(), tick, ix))
        .collect();
}

/// Restart every collector at the CURRENT config tick (Settings ▸ Tick
/// rate is live: the cadence changes the moment the value does).
fn respawn_collector(rt: &mut Rt, tx: &Sender<AppEvent>) {
    for (h, s) in rt.collector.drain(..) {
        s.store(true, Ordering::Relaxed);
        let _ = h.join();
    }
    spawn_all(rt, tx);
}

/// Same true=STOP convention. Follows only NEW bytes (initial tail is the
/// LogState constructor's job, parity).
fn follow_file(
    path: std::path::PathBuf,
    tx: Sender<AppEvent>,
    stop: Arc<AtomicBool>,
    map: fn(String) -> AppEvent,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut reader: Option<io::BufReader<std::fs::File>> = None;
        while !stop.load(Ordering::Relaxed) {
            if reader.is_none() {
                match std::fs::File::open(&path) {
                    Ok(f) => {
                        let mut r = io::BufReader::new(f);
                        let _ = r.seek(SeekFrom::End(0));
                        reader = Some(r);
                    }
                    Err(_) => {
                        thread::sleep(Duration::from_millis(500));
                        continue;
                    }
                }
            }
            let r = reader.as_mut().unwrap();
            let mut line = String::new();
            match r.read_line(&mut line) {
                Ok(1..) => {
                    if tx.send(map(line.trim_end().to_string())).is_err() {
                        break;
                    }
                }
                Ok(_) => thread::sleep(Duration::from_millis(150)),
                Err(_) => {
                    reader = None;
                    thread::sleep(Duration::from_millis(500));
                }
            }
        }
    })
}

fn main() -> io::Result<()> {
    let args: Vec<String> = env::args().collect();
    // -h/--help, -V/--version, unknown-argument rejection (GNU/clap rules).
    if let lsmtop::cli::Decision::Print { stream, text, code } = lsmtop::cli::preflight(&args) {
        use lsmtop::cli::Stream;
        match stream {
            Stream::Out => println!("{text}"),
            Stream::Err => eprintln!("{text}"),
        }
        std::process::exit(code);
    }
    if args.iter().any(|a| a == "--probe-gpu") {
        match lsmtop::gpu::gpu_busy_percent() {
            Some(b) => println!("GPU Busy: {}%", b),
            None => println!("no GPU"),
        }
        return Ok(());
    }
    if let Some(pos) = args.iter().position(|a| a == "--headless") {
        let secs: f64 = args
            .get(pos + 1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(60.0);
        let dump = args
            .iter()
            .position(|a| a == "--dump")
            .and_then(|i| args.get(i + 1))
            .cloned();
        let tname = args
            .iter()
            .position(|a| a == "--target")
            .and_then(|i| args.get(i + 1))
            .cloned();
        return headless(secs, dump.as_deref(), tname.as_deref());
    }
    if let Some(pos) = args.iter().position(|a| a == "--fixture") {
        let dir = args.get(pos + 1).cloned().unwrap_or_else(|| "tests/fixtures".into());
        return headless_fixture(&dir);
    }

    let config_path = Config::default_path();
    let config = Config::load(Some(config_path.clone())).map_err(|e| io::Error::other(e.to_string()))?;
    let opts = OptsState::from_config(&config);
    // one view-state per configured target (multi-server, 2026-11-10)
    let n_targets = config.targets().len().max(1);
    let mut rt = Rt {
        config,
        config_path: config_path.clone(),
        stack: vec![Screen::Metrics],
        target_ix: 0,
        states: (0..n_targets).map(|_| TState::new()).collect(),
        opts,
        graph_scroll: 0,
        graphs_win: None,
        jpane: None,
        jpane_child: None,
        jpane_flags: Vec::new(),
        jpane_mode: "off".to_string(),
        jpane_win: None,
        jtot: None,
        logs: None,
        log_child: None,
        log_flags: Vec::new(),
        collector: Vec::new(),
        journal_last: String::new(),
        lanes_last: String::from("on"),
        unit_infos: HashMap::new(),
        units_polled: Instant::now(),
        dirty: true,
        setst: None,
        step_gate: None,
        hit: Vec::new(),
    };

    enable_raw_mode()?;
    let mut out = io::stdout();
    execute!(out, EnterAlternateScreen)?;
    // must not feel dead to
    // the pointer (maintainer: "Mouse support was previously integral")
    execute!(out, EnableMouseCapture)?;
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), DisableMouseCapture);
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        original_hook(info);
    }));

    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;

    let (tx, rx) = mpsc::channel::<AppEvent>();
    apply_jpane_mode(&mut rt, &tx); // B4: journal pane per config (no-op when off)
    let key_tx = tx.clone();
    let key_stop = Arc::new(AtomicBool::new(false)); // true = stop
    {
        let stop = key_stop.clone();
        thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                if event::poll(Duration::from_millis(100)).unwrap_or(false) {
                    match event::read() {
                        Ok(Event::Key(k)) if key_tx.send(AppEvent::Key(k)).is_err() => break,
                        Ok(Event::Key(_)) => {}
                        Ok(Event::Mouse(m)) if key_tx.send(AppEvent::Mouse(m)).is_err() => break,
                        Ok(Event::Mouse(_)) => {}
                        _ => {}
                    }
                }
            }
        });
    }

    spawn_all(&mut rt, &tx);

    loop {
        if rt.dirty {
            terminal.draw(|f| render(&mut rt, f))?;
            rt.dirty = false;
        }
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(AppEvent::Key(key)) => {
                if lsmtop::input::is_quit_chord(key.code, key.modifiers) {
                    break;
                }
                                // one key law everywhere — law + tests live in src/input.rs
                let code = lsmtop::input::normalize_key(key.code, key.modifiers);
                if !graph_hotkey(&mut rt, code) {
                    let action = App::handle_key(&rt.top(), code);
                    if !handle_action(&mut rt, action, &tx) {
                        break;
                    }
                }
                rt.dirty = true;
            }
            Ok(AppEvent::Mouse(m)) => {
                match m.kind {
                    MouseEventKind::Down(MouseButton::Left) => {
                        if let Some(ix) = hit_at(&rt.hit, m.column, m.row) {
                            let hit = rt.hit[ix].1.clone();
                            match hit {
                                Hit::Key(code) => {
                                    if !graph_hotkey(&mut rt, code) {
                                        let action = App::handle_key(&rt.top(), code);
                                        if !handle_action(&mut rt, action, &tx) {
                                            break;
                                        }
                                    }
                                }
                                h => mouse_hit(&mut rt, h, &tx),
                            }
                            rt.dirty = true;
                        }
                    }
                    // Settings owns the wheel while it is on top (the
                    // dashboard rects below would scroll INVISIBLE panes)
                    MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                        if rt.top() == Screen::Settings && rt.setst.is_some() =>
                    {
                        if let Some(se) = rt.setst.as_mut() {
                            se.scroll = if matches!(m.kind, MouseEventKind::ScrollUp) {
                                se.scroll.saturating_sub(3)
                            } else {
                                se.scroll.saturating_add(3)
                            };
                            rt.dirty = true;
                        }
                    }
                    MouseEventKind::ScrollUp if rt.logs.is_some() => {
                        handle_action(&mut rt, Action::Scroll(-3), &tx);
                        rt.dirty = true;
                    }
                    MouseEventKind::ScrollDown if rt.logs.is_some() => {
                        handle_action(&mut rt, Action::Scroll(3), &tx);
                        rt.dirty = true;
                    }
                    // B4: wheel inside the journal pane parks/follows it
                    MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                        if rt.logs.is_none()
                            && rt.top() == Screen::Metrics
                            && rt.jpane_win.is_some_and(|r| {
                                m.column >= r.x
                                    && m.column < r.x + r.width
                                    && m.row >= r.y
                                    && m.row < r.y + r.height
                            }) =>
                    {
                        if let Some(ls) = rt.jpane.as_mut() {
                            ls.scroll_by(if matches!(m.kind, MouseEventKind::ScrollUp) { 3 } else { -3 });
                            rt.dirty = true;
                        }
                    }
                    // B2: wheel over the graphs window scrolls the pane
                    MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                        if rt.logs.is_none() && rt.top() == Screen::Metrics =>
                    {
                        if let Some((win, max_scroll)) = rt.graphs_win {
                            let over = m.column >= win.x
                                && m.column < win.x + win.width
                                && m.row >= win.y
                                && m.row < win.y + win.height;
                            if over && max_scroll > 0 {
                                rt.graph_scroll = if matches!(m.kind, MouseEventKind::ScrollUp) {
                                    rt.graph_scroll.saturating_sub(3)
                                } else {
                                    (rt.graph_scroll + 3).min(max_scroll)
                                };
                                rt.dirty = true;
                            }
                        }
                    }
                    _ => {}
                }
            }
            Ok(AppEvent::Sample(ix, Ok(data))) => {
                // route by TARGET: every server keeps its own sampler,
                // listing and ghosts; the GPU rings are this machine's
                // truth and ride the ACTIVE target's ticks only.
                let gpu_on = rt.opts.value("gpu_graph") == "on";
                let active = ix == rt.target_ix;
                if active && rt.units_polled.elapsed() >= Duration::from_secs(1) {
                    poll_units(&mut rt); // ~1 Hz while samples flow
                }
                {
                    let i = ix.min(rt.states.len() - 1);
                    let st = &mut rt.states[i];
                    if let SampleData::Router(rs) = &data {
                        st.last_models = Some(rs.models.clone());
                    }
                    // the scrape STRATEGY that produced this sample is the
                    // engine truth (collector self-heals it from payloads)
                    st.mode = match &data {
                        SampleData::Router(_) => EngineMode::Router,
                        SampleData::Server(_) => EngineMode::Single,
                    };
                    // v9.15: 'off stops all GPU polling' — poll ONLY while on;
                    // gpu_busy is set before ingest so the apu ring stays
                    // tick-aligned with gen/prompt/kv.
                    if active && gpu_on {
                        st.sampler.gpu_busy = lsmtop::gpu::gpu_busy_percent().map(|v| v as f64);
                        st.sampler.gpu_clk = lsmtop::gpu::gpu_clock_mhz();
                        st.sampler.gpu_pwr = lsmtop::gpu::gpu_power_w();
                    } else if active {
                        st.sampler.gpu_busy = None;
                    }
                    st.sampler.ingest(now_secs(), &data);
                    st.last_sample_err = None;
                }
                rt.dirty = true;
            }
            Ok(AppEvent::Sample(ix, Err(e))) => {
                let gpu_on = rt.opts.value("gpu_graph") == "on";
                let active = ix == rt.target_ix;
                {
                    let i = ix.min(rt.states.len() - 1);
                    let st = &mut rt.states[i];
                    st.sampler.ingest_error(&e.0);
                    st.last_sample_err = Some(e.0);
                    if st.sampler.err_streak >= 2 {
                        // Time continues over a dead target (maintainer 2026-11-09):
                        // rail samples from the 2nd consecutive error; the GPU
                        // keeps polling wherever gpu_graph says — the card is
                        // still busy even with the llama server down.
                        if active && gpu_on {
                            st.sampler.gpu_busy = lsmtop::gpu::gpu_busy_percent().map(|v| v as f64);
                            st.sampler.gpu_clk = lsmtop::gpu::gpu_clock_mhz();
                            st.sampler.gpu_pwr = lsmtop::gpu::gpu_power_w();
                        } else if active {
                            st.sampler.gpu_busy = None;
                        }
                        st.sampler.ingest_unreachable();
                    }
                    if st.sampler.err_streak >= 3 {
                        // Down ~3 ticks: empty the table honestly — dashes +
                        // `no model`, banner 0/0. The metrics of a dead server
                        // must not park as if live (maintainer: metrics is correct
                        // data). Next good scrape overwrites both; recovery is
                        // automatic and a transient blip never reaches 3.
                        st.sampler.rows.clear();
                        st.lane_alive.clear(); // a dead server owns no ghosts
                        st.last_models = Some(serde_json::json!({"data": []}));
                    }
                }
                rt.dirty = true;
            }
            Ok(AppEvent::ScrapeError(m)) => {
                let st = rt.ts_mut();
                st.sampler.ingest_error(&m);
                st.last_sample_err = Some(m);
                rt.dirty = true;
            }
            Ok(AppEvent::LogLine(l)) => {
                if let Some(ev) = lsmtop::sampling::parse_journal_total(&l) {
                    lsmtop::sampling::apply_journal_total(&mut rt.jtot, ev);
                    rt.dirty = true;
                }
                if let Some(ls) = rt.logs.as_mut() {
                    ls.push_line(&l);
                    rt.dirty = true;
                }
            }
            Ok(AppEvent::JPaneLine(l)) => {
                if let Some(ev) = lsmtop::sampling::parse_journal_total(&l) {
                    lsmtop::sampling::apply_journal_total(&mut rt.jtot, ev);
                    rt.dirty = true;
                }
                if let Some(ls) = rt.jpane.as_mut() {
                    ls.push_line(&l);
                    rt.dirty = true;
                }
            }
            Ok(AppEvent::Tick) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    // graceful quit: ALL collectors and journal followers die.
    for (h, s) in rt.collector.drain(..) {
        s.store(true, Ordering::Relaxed);
        let _ = h.join();
    }
    close_logs(&mut rt);
    key_stop.store(true, Ordering::Relaxed);
    disable_raw_mode()?;
    let _ = execute!(terminal.backend_mut(), DisableMouseCapture);
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

/// M9.4/M9.1: headless collection — no terminal, real collector cadence,
/// per-tick JSON timeline dumped for the parity/soak harnesses.
fn rss_kib() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmRSS:"))
                .and_then(|l| l.split_whitespace().nth(1).and_then(|v| v.parse().ok()))
        })
        .unwrap_or(0)
}

fn headless(secs: f64, dump: Option<&str>, tname: Option<&str>) -> io::Result<()> {
    let path = Config::default_path();
    let config = Config::load(Some(path)).map_err(|e| io::Error::other(e.to_string()))?;
    let tick = config.tick_ms();
    let targets = config.targets();
    let target = targets
        .iter()
        .find(|t| {
            tname
                .map(|n| t.get("name").and_then(serde_json::Value::as_str) == Some(n))
                .unwrap_or(false)
        })
        .cloned()
        .or_else(|| targets.into_iter().next())
        .ok_or_else(|| io::Error::other("no targets configured"))?;
    let (tx, rx) = mpsc::channel();
    let (handle, stop) = spawn_collector(target.clone(), tx.clone(), tick, 0);
    let mut sampler = Sampler::new();
    let t0 = Instant::now();
    let wall0 = now_secs();
    let rss0 = rss_kib();
    let mut events = 0u64;
    let mut errors = 0u64;
    let mut ticks: Vec<serde_json::Value> = Vec::new();
    while t0.elapsed().as_secs_f64() < secs {
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(AppEvent::Sample(_, Ok(data))) => {
                let ts = now_secs();
                sampler.ingest(ts, &data);
                events += 1;
                if dump.is_some() {
                let rowj: Vec<serde_json::Value> = sampler
                    .rows
                    .iter()
                    .map(|r| {
                        let phase = sampler
                            .last_live
                            .get(&r.model)
                            .and_then(|o| o.as_ref())
                            .map(|l| l.phase);
                        serde_json::json!({
                            "model": r.model, "gen_ts": r.gen_ts, "prompt_ts": r.prompt_ts,
                            "cache_pct": r.cache_pct, "kv": r.kv, "active": r.active,
                            "phase": phase,
                        })
                    })
                    .collect();
                let one = |mid: &str, l: &lsmtop::net::LaneSample| {
                    let ctr = |m: Option<&std::collections::HashMap<String, f64>>| {
                        m.map(|mm| {
                            serde_json::json!({
                                "pred": mm.get("llamacpp:tokens_predicted_total"),
                                "pred_s": mm.get("llamacpp:tokens_predicted_seconds_total"),
                                "pr": mm.get("llamacpp:prompt_tokens_total"),
                                "pr_s": mm.get("llamacpp:prompt_seconds_total"),
                                "dec": mm.get("llamacpp:n_decode_total"),
                            })
                        })
                    };
                    let slim = l.slots.as_ref().and_then(|s| s.as_array()).map(|arr| {
                        arr.iter()
                            .map(|sl| serde_json::json!({
                                "id": sl.get("id"),
                                "proc": sl.get("is_processing"),
                                "pt": sl.get("n_prompt_tokens"),
                                "pp": sl.get("n_prompt_tokens_processed"),
                                "g": sl
                                    .get("n_generated_tokens")
                                    .or_else(|| sl.get("next_token").and_then(read_ndecoded)),
                            }))
                            .collect::<Vec<_>>()
                    });
                    serde_json::json!({
                        "id": mid,
                        "slots": slim,
                        "ctr": ctr(l.metrics.as_ref()),
                    })
                };
                let raw: Vec<serde_json::Value> = match &data {
                    SampleData::Router(r) => r.lanes.iter().map(|(m, l)| one(m, l)).collect(),
                    SampleData::Server(s) => vec![one(
                        "server",
                        &lsmtop::net::LaneSample {
                            slots: s.slots.clone(),
                            metrics: s.metrics.clone(),
                        },
                    )],
                };
                    ticks.push(serde_json::json!({
                        "ts": ts, "rows": rowj,
                        "raw": raw,
                        "curve": [
                            sampler.gen.values().last().copied().unwrap_or(0.0),
                            sampler.prompt.values().last().copied().unwrap_or(0.0),
                        ],
                    }));
                }
            }
            Ok(AppEvent::Sample(_, Err(e))) => {
                sampler.ingest_error(&e.0);
                errors += 1;
            }
            Ok(AppEvent::ScrapeError(m)) => {
                sampler.ingest_error(&m);
                errors += 1;
            }
            Ok(_) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    stop.store(true, Ordering::Relaxed);
    handle
        .join()
        .map_err(|_| io::Error::other("collector thread panicked"))?;
    let end_rows: Vec<serde_json::Value> = sampler
        .rows
        .iter()
        .map(|r| {
            serde_json::json!({"model": r.model, "gen_ts": r.gen_ts,
                               "prompt_ts": r.prompt_ts, "cache_pct": r.cache_pct,
                               "kv": r.kv, "active": r.active})
        })
        .collect();
    let out = serde_json::json!({
        "target": target.get("name").cloned().unwrap_or(serde_json::Value::Null),
        "seconds": secs, "wall_start": wall0, "wall_end": now_secs(),
        "events": events, "errors": errors, "polls": sampler.polls,
        "tick_ms": tick,
        "rss_kib_start": rss0, "rss_kib_end": rss_kib(),
        "rows": end_rows, "ticks": ticks,
    });
    if let Some(p) = dump {
        std::fs::write(p, serde_json::to_string(&out)?)?;
    }
    println!(
        "headless done: polls {} events {} errors {} rss {}->{} KiB",
        sampler.polls,
        events,
        errors,
        rss0,
        rss_kib()
    );
    Ok(())
}

/// Settings ring state. The screen is HOTKEY-FIRST and has no text fields
/// anymore (server binary / models dir / HF token left with their features):
/// the ring walks the tick-rate row (0) and the embedded option rows (1+),
/// enter/space or ←/→ act on the focused row directly.
#[derive(Clone, Default)]
struct SetFocus {
    focus: usize,
    /// which tick button the keyboard highlight sits on: 0 = [ + ], 1 = [ - ]
    tick_sel: usize,
    /// content scroll (rows hidden above the fold; wheel + focus-follow)
    scroll: u16,
}

/// Settings content row law — ONE source (the render loop below mirrors it
/// exactly): 0 section header, 1 blank, then per option row i a chip row at
/// 2+2i followed by a BLANK line (maintainer 2026-11-07: restore a space line
/// between graph options — back-to-back buttons read too tight). Row 0 of
/// the ring is the tick-rate row; rows 1..=N are the option chips.
/// Content length = 2 + 2*N.
pub fn set_focus_rows(focus: usize) -> (u16, u16) {
    let row = 2 + 2 * focus as u16;
    (row, row)
}

/// focus-follow: keep the focused span inside the window [sc, sc+vis),
/// then clamp against the content bottom.
pub fn follow_scroll(scroll: u16, span: (u16, u16), vis: u16, content_len: u16) -> u16 {
    let mut sc = scroll;
    if span.0 < sc {
        sc = span.0;
    }
    if span.1 + 1 > sc + vis {
        sc = span.1 + 1 - vis;
    }
    clamp_scroll(sc, content_len, vis)
}

pub fn clamp_scroll(scroll: u16, content_len: u16, vis: u16) -> u16 {
    scroll.min(content_len.saturating_sub(vis))
}

/// what a mouse press lands on; rects are recorded during draw
#[derive(Clone, PartialEq)]
enum Hit {
    OptsChip(usize, usize),
    /// the tick-rate row's [ + ] / [ - ] button
    TickBtn(isize),
    /// anywhere else on a Settings row: focus it
    RowFocus(usize),
    /// a footer/hotkey-strip token: pressing it IS pressing the key
    Key(KeyCode),
}

fn hit_at(hits: &[(Rect, Hit)], x: u16, y: u16) -> Option<usize> {
    hits.iter().rposition(|(r, _)| x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height)
}

/// Settings routing: every row answers the same keys — ↑/↓ walk the ring,
/// enter/space PRESS the focused row (cycle a chip forward, or fire the
/// highlighted tick button), ←/→ act within the row (cycle a chip by one,
/// or slide the tick highlight between [ + ] and [ - ]). q is Back.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Route {
    Move(isize),
    /// cycle the chip row under focus (dir = ±1; +1 == the enter "press")
    CycleChip(isize),
    /// slide the tick [ + ]/[ - ] highlight (dir = ±1)
    TickSel(isize),
    /// fire the highlighted tick button (through the repeat gate)
    TickPress,
    EscBack,
    Ignore,
}

fn route_settings(sk: &SetKey, focus: usize) -> Route {
    match sk {
        SetKey::NextField => Route::Move(1),
        SetKey::PrevField => Route::Move(-1),
        SetKey::Activate => {
            if focus == 0 {
                Route::TickPress
            } else {
                Route::CycleChip(1)
            }
        }
        SetKey::ChipDir(d) => {
            if focus == 0 {
                Route::TickSel(*d)
            } else {
                Route::CycleChip(*d)
            }
        }
        SetKey::Char(c) => {
            if *c == 'q' {
                Route::EscBack
            } else {
                Route::Ignore
            }
        }
    }
}

fn open_settings(rt: &mut Rt) {
    rt.setst = Some(SetFocus { focus: 0, tick_sel: 0, scroll: 0 });
    rt.step_gate = None;
    rt.stack.push(Screen::Settings);
}

/// R3: the metrics-options chips have no Save mechanic — every change
/// persists to config the moment it happens (graphs re-read opts from the
/// live OptsState each tick; the journal pane follows through its mode).
fn opts_live_save(rt: &mut Rt, tx: &Sender<AppEvent>) {
    if let Err(e) = rt.opts.save(&mut rt.config, &rt.config_path.clone()) {
        rt.say(format!("save failed: {e}"));
    }
    apply_jpane_mode(rt, tx);
}

// ───────────────────────── tick-rate key-repeat gate ─────────────────────────
// Terminal key repeat fires ~30/s — raw repeats would tear the value across
// its range. Law: the FIRST press applies at once; repeats that arrive while
// the key is still held (events < COALESCE apart) are SWALLOWED until the
// hold passes DELAY, then applied at most every GAP. A tap is one 50 ms
// step; a hold glides. Pure over an injectable ms clock, unit-proven.
#[derive(Clone, Copy, PartialEq, Debug)]
struct StepGate {
    first: u64,
    last_evt: u64,
    last_apply: u64,
    dir: i64,
}

const GATE_COALESCE_MS: u64 = 400; // silence past this = the next press is fresh
const GATE_DELAY_MS: u64 = 500; // a hold must age this long before repeating
const GATE_GAP_MS: u64 = 120; // once repeating, one step per GAP at most

fn step_gate_allow(now: u64, st: &mut Option<StepGate>, dir: isize) -> bool {
    let d = dir as i64;
    let held = st.filter(|g| g.dir == d && now.saturating_sub(g.last_evt) <= GATE_COALESCE_MS);
    let allow = match held {
        None => true, // fresh press (or a direction flip)
        Some(g) => {
            now.saturating_sub(g.first) >= GATE_DELAY_MS
                && now.saturating_sub(g.last_apply) >= GATE_GAP_MS
        }
    };
    let first = match held {
        Some(g) => g.first,
        None => now,
    };
    let last_apply = if allow { now } else { held.map(|g| g.last_apply).unwrap_or(now) };
    *st = Some(StepGate { first, last_evt: now, last_apply, dir: d });
    allow
}

/// Apply one tick step: clamp on the 500..5000 grid, persist, and restart
/// the collector at the new cadence. Mouse clicks bypass the gate (they are
/// discrete); the keyboard path filters through step_gate_allow first.
fn tick_step(rt: &mut Rt, dir: isize, tx: &Sender<AppEvent>) {
    let cur = rt.config.tick_ms();
    let Some(next) = tick_step_from(cur, dir) else { return };
    rt.config.set_tick_ms(next);
    if let Err(e) = rt.config.save(&rt.config_path.clone()) {
        rt.say(format!("tick save failed: {e}"));
    }
    respawn_collector(rt, tx);
}

/// a mouse press decoded into intent, applied
fn mouse_hit(rt: &mut Rt, hit: Hit, tx: &Sender<AppEvent>) {
    match hit {
        // footer tokens are dispatched by the caller as keypresses
        Hit::Key(_) => {}
        Hit::RowFocus(ix) => {
            if let Some(se) = rt.setst.as_mut() {
                se.focus = ix;
            }
            rt.step_gate = None;
        }
        Hit::TickBtn(dir) => {
            if let Some(se) = rt.setst.as_mut() {
                // clicking a button focuses the row and lands the highlight
                // on the button that was pressed
                se.focus = 0;
                se.tick_sel = if dir > 0 { 0 } else { 1 };
            }
            rt.step_gate = None;
            tick_step(rt, dir, tx);
        }
        Hit::OptsChip(row, idx) => {
            if let Some(r) = rt.opts.rows.get_mut(row) {
                r.press(idx);
                rt.opts.sync_disabled();
            }
            opts_live_save(rt, tx); // R3: a clicked chip persists at once
        }
    }
}

fn close_logs(rt: &mut Rt) {
    if let Some(mut c) = rt.log_child.take() {
        let _ = c.kill();
        let _ = c.wait();
    }
    for f in rt.log_flags.drain(..) {
        f.store(true, Ordering::Relaxed);
    }
    rt.logs = None;
}

fn poll_units(rt: &mut Rt) {
    rt.units_polled = Instant::now();
    let runner = RealRunner;
    for (i, t) in rt.config.targets().iter().enumerate() {
        let unit = t.get("unit").and_then(serde_json::Value::as_str).unwrap_or("");
        if unit.is_empty() {
            continue;
        }
        let scope = t.get("scope").and_then(serde_json::Value::as_str).unwrap_or("system");
        let r = systemctl_show(&runner, unit, scope, Duration::from_millis(800));
        rt.unit_infos.insert(i, r);
    }
}

/// B4: bring the journal lower pane to the configured size (Settings >
/// Metrics > journal). Attaches journalctl -f for the FIRST target that
/// declares a systemd unit; the pane keeps its own scroll and follows
/// the live bottom until the wheel parks it.
fn apply_jpane_mode(rt: &mut Rt, tx: &Sender<AppEvent>) {
    let mode = rt.config.metrics().journal;
    if mode == rt.jpane_mode && (mode == "off") == rt.jpane.is_none() {
        return;
    }
    rt.jpane_mode = mode.clone();
    // any previous pane follower stops here (journal child + owned tails)
    for f in rt.jpane_flags.drain(..) {
        f.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    if let Some(mut c) = rt.jpane_child.take() {
        let _ = c.kill();
        let _ = c.wait();
    }
    if mode == "off" {
        rt.jpane = None;
        rt.jpane_win = None;
        rt.dirty = true;
        return;
    }
    let unit_targets: Vec<_> = rt.config.targets();
    let t = unit_targets
        .iter()
        .find(|t| !t.get("unit").and_then(serde_json::Value::as_str).unwrap_or("").is_empty())
        .or_else(|| unit_targets.first())
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let name = t.get("name").and_then(serde_json::Value::as_str).unwrap_or("target").to_string();
    let unit = t.get("unit").and_then(serde_json::Value::as_str).unwrap_or("").to_string();
    let scope = t.get("scope").and_then(serde_json::Value::as_str).unwrap_or("system").to_string();
    if unit.is_empty() {
        // B4 mirrors the full-screen journal law: a unit-less target owns
        // a plain log file (svc-<ix>.log) — tail THAT into the pane so
        // the single tier can ride the newest entry whole.
        let ix = rt.target_ix;
        let port = t.get("port").and_then(serde_json::Value::as_u64).unwrap_or(0) as u16;
        let path = logs_dir().join(format!("svc-{ix}.log"));
        if path.exists() {
            let ls = LogState::new(LogSource::OwnedFile { name, port, path: path.clone() });
            let stop = Arc::new(AtomicBool::new(false));
            follow_file(path, tx.clone(), stop.clone(), AppEvent::JPaneLine);
            rt.jpane_flags.push(stop);
            rt.jpane = Some(ls);
            rt.dirty = true;
            return;
        }
        let mut ls = LogState::new(LogSource::Journal { name, unit: String::new(), scope });
        ls.push_plain("journal pane: no unit target configured");
        rt.jpane = Some(ls);
        rt.dirty = true;
        return;
    }
    let mut ls = LogState::new(LogSource::Journal { name, unit: unit.clone(), scope: scope.clone() });
    ls.push_plain("\u{2014} journal pane attaching \u{00b7} wheel parks the tail \u{2014}");
    rt.jpane = Some(ls);
    let argv = journal_cmd(&unit, &scope);
    match Command::new(&argv[0]).args(&argv[1..]).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn() {
        Ok(mut child) => {
            let ktx = tx.clone();
            let so = child.stdout.take();
            let se = child.stderr.take();
            if let Some(e) = se {
                let etx = ktx.clone();
                thread::spawn(move || {
                    for l in io::BufReader::new(e).lines().map_while(Result::ok) {
                        let _ = etx.send(AppEvent::JPaneLine(l));
                    }
                });
            }
            thread::spawn(move || {
                if let Some(o) = so {
                    for l in io::BufReader::new(o).lines().map_while(Result::ok) {
                        if ktx.send(AppEvent::JPaneLine(l)).is_err() {
                            break;
                        }
                    }
                }
            });
            rt.jpane_child = Some(child);
        }
        Err(e) => {
            if let Some(ls) = rt.jpane.as_mut() {
                ls.push_plain(&format!("journalctl not available on this host ({e})"));
            }
        }
    }
    rt.dirty = true;
}

fn open_logs(rt: &mut Rt, tx: Sender<AppEvent>) {
    close_logs(rt);
    let t = rt.target();
    let name = t.get("name").and_then(serde_json::Value::as_str).unwrap_or("target").to_string();
    let unit = t.get("unit").and_then(serde_json::Value::as_str).unwrap_or("").to_string();
    let scope = t.get("scope").and_then(serde_json::Value::as_str).unwrap_or("system").to_string();
    if !unit.is_empty() {
        let mut ls =
            LogState::new(LogSource::Journal { name, unit: unit.clone(), scope: scope.clone() });
        ls.push_plain("— attaching to journalctl -f —");
        rt.logs = Some(ls);
        let argv = journal_cmd(&unit, &scope);
        match Command::new(&argv[0])
            .args(&argv[1..])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(mut child) => {
                let ktx = tx.clone();
                let so = child.stdout.take();
                let se = child.stderr.take();
                // merges stderr=STDOUT into ONE stream; std cannot
                // merge post-spawn — draining stderr FIRST on the stdout
                // thread deadlocked forever (journalctl -f keeps stderr
                // open), which is why the pane sat at "attaching".
                if let Some(e) = se {
                    let etx = ktx.clone();
                    thread::spawn(move || {
                        for l in io::BufReader::new(e).lines().map_while(Result::ok) {
                            let _ = etx.send(AppEvent::LogLine(l));
                        }
                    });
                }
                thread::spawn(move || {
                    if let Some(o) = so {
                        for l in io::BufReader::new(o).lines().map_while(Result::ok) {
                            if ktx.send(AppEvent::LogLine(l)).is_err() {
                                break;
                            }
                        }
                    }
                });
                rt.log_child = Some(child);
            }
            Err(e) => {
                if let Some(ls) = rt.logs.as_mut() {
                    ls.push_plain(&format!("journalctl not available on this host ({e})"));
                }
            }
        }
    } else {
        let ix = rt.target_ix;
        let port = t.get("port").and_then(serde_json::Value::as_u64).unwrap_or(0) as u16;
        let path = logs_dir().join(format!("svc-{ix}.log"));
        rt.logs = Some(LogState::new(LogSource::OwnedFile { name, port, path: path.clone() }));
        let stop = Arc::new(AtomicBool::new(false));
        let h = follow_file(path, tx.clone(), stop.clone(), AppEvent::LogLine);
        rt.log_flags.push(stop);
        let _ = h; // joined implicitly via flag at close_logs; handle kept simple
    }
}

/// returns false → quit
fn handle_action(rt: &mut Rt, action: Action, tx: &Sender<AppEvent>) -> bool {
    match action {
        Action::Quit => {
            if rt.top() == Screen::Logs {
                close_logs(rt);
            }
            false
        }
        Action::GoBack => {
            // R7: esc/q back out of every nested screen; only the metrics
            // root's q quits. Chips and the tick value already match the
            // config on leaving Settings — there is nothing to cancel.
            if rt.top() == Screen::Logs {
                close_logs(rt);
            }
            if rt.stack.len() > 1 {
                rt.stack.pop();
            }
            rt.step_gate = None;
            true
        }
        Action::SwitchScreen(s) => {
            if s == Screen::Settings {
                open_settings(rt);
                return true;
            }
            rt.stack.push(s);
            true
        }
        Action::LanesSize => {
            // l: table SIZE, the unified law — on (active only) <-> all,
            // the cycle SKIPS off; from off it turns the pane back on.
            let cur = rt.config.metrics().lanes;
            let next = lsmtop::config::lanes_size_next(&cur);
            rt.lanes_last = next.to_string();
            if let Err(e) = rt.config.set_metric_opts(&[("lanes", next)]) {
                rt.say(&e);
            } else {
                let _ = rt.config.save(&rt.config_path.clone());
            }
            true
        }
        Action::LanesVis => {
            // L: table VISIBILITY only — on/all -> off -> back to the last
            // size seen. Mirrors the journal J rule exactly.
            let cur = rt.config.metrics().lanes;
            if cur != "off" {
                rt.lanes_last = cur.clone();
            }
            let next = lsmtop::config::lanes_vis_next(&cur, &rt.lanes_last);
            if let Err(e) = rt.config.set_metric_opts(&[("lanes", next)]) {
                rt.say(&e);
            } else {
                let _ = rt.config.save(&rt.config_path.clone());
            }
            true
        }
        Action::JournalToggle => {
            // J: journal VISIBILITY toggle — any tier (or the full screen)
            // goes OFF; from off it returns to the tier it was last on
            // (default four). The j cycle owns tier selection, J owns
            // visibility; both persist.
            let on_full = rt.top() == Screen::Logs;
            let cur = rt.config.metrics().journal;
            let next = if cur == "off" {
                if rt.journal_last.is_empty() {
                    "four".to_string()
                } else {
                    std::mem::take(&mut rt.journal_last)
                }
            } else {
                rt.journal_last = cur.clone();
                "off".to_string()
            };
            if let Err(e) = rt.config.set_metric_opts(&[("journal", next.as_str())]) {
                rt.say(&e);
            } else if let Err(e) = rt.config.save(&rt.config_path.clone()) {
                rt.say(format!("journal {next} (save failed: {e})"));
            }
            rt.opts = OptsState::from_config(&rt.config);
            apply_jpane_mode(rt, tx);
            if on_full && next == "off" {
                close_logs(rt);
                if rt.stack.len() > 1 {
                    rt.stack.pop();
                }
            }
            true
        }
        Action::JournalCycle => {
            // 'j' walks the journal on EVERY screen it reaches: off -> four
            // -> ten -> FULL-SCREEN journal, and on the full screen j CLOSES
            // it and the cycle restarts at four (maintainer: "esc Back is no
            // longer needed"). "off" is set by the chip or by the J toggle.
            let on_full = rt.top() == Screen::Logs;
            let cur = rt.config.metrics().journal;
            let (next, open_full) = lsmtop::config::journal_next(&cur, on_full);
            if let Err(e) = rt.config.set_metric_opts(&[("journal", next)]) {
                rt.say(&e);
            } else {
                match rt.config.save(&rt.config_path.clone()) {
                    Ok(()) => rt.say(format!(
                        "journal {next}{}",
                        if open_full { " · full screen" } else { "" }
                    )),
                    Err(e) => rt.say(format!("journal {next} (save failed: {e})")),
                }
            }
            rt.opts = OptsState::from_config(&rt.config);
            apply_jpane_mode(rt, tx);
            if on_full {
                close_logs(rt);
                if rt.stack.len() > 1 {
                    rt.stack.pop();
                }
            } else if open_full {
                open_logs(rt, tx.clone());
                rt.stack.push(Screen::Logs);
            }
            true
        }
        Action::NextTarget => {
            rt.target_ix = lsmtop::input::next_target(rt.target_ix, rt.states.len());
            true
        }
        Action::Clear => {
            if let Some(ls) = rt.logs.as_mut() {
                ls.clear_view();
            }
            true
        }
        Action::SetEdit(sk) => {
            let focus = rt.setst.as_ref().map(|s| s.focus).unwrap_or(0);
            match route_settings(&sk, focus) {
                Route::Move(d) => {
                    if let Some(se) = rt.setst.as_mut() {
                        // ring = tick row + every embedded option row
                        let n = (1 + rt.opts.rows.len()) as isize;
                        se.focus = ((focus as isize + d).rem_euclid(n)) as usize;
                    }
                    rt.step_gate = None;
                }
                Route::CycleChip(d) => {
                    if let Some(r) = rt.opts.rows.get_mut(focus.saturating_sub(1)) {
                        r.cycle(d);
                    }
                    rt.opts.sync_disabled();
                    opts_live_save(rt, tx); // R3: chips persist at once
                }
                Route::TickSel(d) => {
                    if let Some(se) = rt.setst.as_mut() {
                        se.tick_sel = ((se.tick_sel as isize + d).clamp(0, 1)) as usize;
                    }
                }
                Route::TickPress => {
                    let dir = if rt.setst.as_ref().map(|s| s.tick_sel).unwrap_or(0) == 0 {
                        1
                    } else {
                        -1
                    };
                    if step_gate_allow(now_ms(), &mut rt.step_gate, dir) {
                        tick_step(rt, dir, tx);
                    }
                }
                Route::EscBack => {
                    if rt.stack.len() > 1 {
                        rt.stack.pop();
                    }
                    rt.step_gate = None;
                    return true;
                }
                Route::Ignore => {}
            }
            true
        }
        Action::Scroll(d) => {
            if let Some(ls) = rt.logs.as_mut() {
                ls.scroll_by(if d == isize::MIN { -1_000_000 } else { d });
            }
            true
        }
        Action::Noop => true,
    }
}

/// G2: graph letters act DIRECTLY on the metrics screen — no focus mode.
/// Lowercase cycles that pane's size (full \u{2192} half \u{2192} single), Shift
/// toggles its visibility; g = GEN (Llama moved to 'L'), u = GPU. Every
/// change persists through config, same validated metric gate as Settings.
/// (Off the hotkey strip since the 2026-11-07 chrome pass — cataloged in
/// help.md, still live keys.)
fn graph_hotkey(rt: &mut Rt, code: KeyCode) -> bool {
    if rt.top() != Screen::Metrics {
        return false;
    }
    let (cfg_key, name, kind) = match lsmtop::input::graph_hotkey_map(code) {
        Some(v) => v,
        None => return false,
    };
    let what = match kind {
        lsmtop::input::HotKind::Size => "size",
        lsmtop::input::HotKind::Visibility => "graph",
    };
    let res = if what == "size" {
        rt.config.cycle_metric(cfg_key)
    } else {
        rt.config.toggle_metric(cfg_key)
    };
    match res {
        Ok(next) => {
            let msg = match rt.config.save(&rt.config_path.clone()) {
                Ok(()) => format!("{name} {what} {next}"),
                Err(e) => format!("{name} change applied, save failed: {e}"),
            };
            rt.opts = OptsState::from_config(&rt.config);
            rt.say(&msg);
        }
        Err(e) => rt.say(&e),
    }
    true
}

fn apu_title() -> &'static str {
    if lsmtop::gpu::gpu_backend() == "amdgpu" {
        "APU %"
    } else {
        "GPU %"
    }
}

/// Banner tail slot: a systemd-unit target rides its unit state + PID
/// (2026-11-07 wording, harness-pinned); a bare target rides the engine
/// mode its endpoint REVEALED — a measured fact, never `? · PID 0` noise
/// for single-instance users (multi-server pass, 2026-11-10). Header law
/// holds: the banner states metrics, never directions.
fn banner_detail(unit: Option<(&str, Option<u64>)>, mode: EngineMode) -> String {
    match unit {
        Some((state, pid)) => format!("{state} \u{00b7} PID {}", pid.unwrap_or(0)),
        None => match mode {
            EngineMode::Router => "router".to_string(),
            EngineMode::Single => "single".to_string(),
            EngineMode::Unknown => "auto".to_string(),
        },
    }
}

#[cfg(test)]
mod banner_tests {
    use super::{banner_detail, EngineMode};

    #[test]
    fn unit_wording_is_untouched() {
        assert_eq!(
            banner_detail(Some(("active", Some(1872))), EngineMode::Router),
            "active \u{00b7} PID 1872"
        );
        // a unit that answers no PID still reads PID 0 (old shape)
        assert_eq!(
            banner_detail(Some(("inactive", None)), EngineMode::Single),
            "inactive \u{00b7} PID 0"
        );
    }

    #[test]
    fn bare_target_rides_its_detected_mode() {
        assert_eq!(banner_detail(None, EngineMode::Single), "single");
        assert_eq!(banner_detail(None, EngineMode::Router), "router");
        // nothing answered yet: honest 'auto', never '? · PID 0'
        assert_eq!(banner_detail(None, EngineMode::Unknown), "auto");
    }
}

/// B1 cell law: running-total tokens, compact. Blank (empty string) when
/// no counter is live — the column must EMPTY, never freeze a stale number.
fn fmt_total_tk(v: Option<f64>) -> String {
    match v {
        None => String::new(),
        Some(v) if v >= 99_950.0 => format!("{:.0}k", v / 1000.0),
        Some(v) if v >= 9_995.0 => format!("{:.1}k", v / 1000.0),
        Some(v) => format!("{v:.0}"),
    }
}

fn table_cells(r: &lsmtop::sampling::LaneRow, jt: Option<&lsmtop::sampling::JournalTotal>) -> [String; 9] {
    // exact formats; falsy rates render the em dash. Units ride the cells now (maintainer 2026-11-09).
    // B1: TOTAL tk rides between PROMPT and GEN.
    [
        r.model.clone(),
        r.act.clone(),
        if r.prompt_ts > 0.0 { lsmtop::ui::dashboard::fmt_rate(r.prompt_ts) } else { "\u{2014}".into() },
        match jt {
            // decode: running n_gen; prompt: running n_tokens + known %
            Some(j) if j.decoding && j.gen.is_some() => fmt_total_tk(j.gen),
            Some(j) if !j.decoding && j.prompt_n.is_some() => format!(
                "{} {:.0}%",
                fmt_total_tk(j.prompt_n),
                j.prompt_pct.unwrap_or(0.0) * 100.0
            ),
            // No journal line for this run (bare server, journal off):
            // the slot counters ARE the running totals (maintainer 2026-11-10).
            _ if r.total_tk.is_some() => fmt_total_tk(r.total_tk),
            _ => "\u{2014}".into(),
        },
        if r.gen_ts > 0.0 { lsmtop::ui::dashboard::fmt_rate(r.gen_ts) } else { "\u{2014}".into() },
        format!("{:.1}%", r.kv), // CACHE: % attached to the number (maintainer 2026-11-09)
        r.cache_pct.map(|c| format!("{:.0}%", c)).unwrap_or_else(|| "\u{2014}".into()),
        r.ctx_k.map(|k| format!("{k}k")).unwrap_or_else(|| "\u{2014}".into()),
        r.acc.map(|a| format!("{:.0}%", a)).unwrap_or_else(|| "\u{2014}".into()),
    ]
}

/// (rows, loaded_n, total_n) — table iterates /models status; loaded lanes
/// ride the sampler rows, unloaded show dashes with the status string
/// parked in ACCEPT; `show_all` lists everything. R8 lane-alive: `alive`
/// (caller-owned, sticky) remembers every id that was ever loaded or
/// swapping — those lanes keep a ghost row forever after, so the box never
/// changes size; only a lane set that was never known reads `no model`.
/// focus-pin law (maintainer 2026-11-10): the lane the curve follows rides the
/// TOP LANES row — there its status word reads `loaded`, and that is the
/// focus marker. Other lanes keep listing order; no focus moves nothing.
fn pin_focus_lanes(lanes: &[(String, String)], focus: Option<&str>) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..lanes.len()).collect();
    if let Some(f) = focus {
        if let Some(pos) = idx.iter().position(|&i| lanes[i].0 == f) {
            let id = idx.remove(pos);
            idx.insert(0, id);
        }
    }
    idx
}

fn build_table_rows(
    models: Option<&serde_json::Value>,
    rows: &[lsmtop::sampling::LaneRow],
    show_all: bool,
    jt: Option<&lsmtop::sampling::JournalTotal>,
    alive: &mut Vec<String>,
    focus: Option<&str>,
) -> (Vec<DashRow>, usize, usize) {
    let mut out = Vec::new();
    let (mut loaded_n, mut total_n) = (0usize, 0usize);
    // llama-swap answers {"data":[...]} — digging only here (not the bare
    // array fallback) is what made the banner read 1/1 next to  1/11.
    let arr_opt = models.and_then(|m| {
        m.get("data")
            .and_then(|d| d.as_array())
            .or_else(|| m.as_array())
    });
    if let Some(arr) = arr_opt {
        // Pass 1: read the snapshot's truth BEFORE painting (R9, maintainer
        // 2026-11-10): a ghost row exists ONLY while NOTHING is loaded — it
        // holds the box open at idle. Any live lane (loaded OR swapping —
        // a swap is the act of loading, Task-4 name law) evicts every ghost
        // permanently, so the list never rides live+ghost side by side.
        let lanes: Vec<(String, String)> = arr
            .iter()
            .map(|m| {
                let mid = m.get("id").and_then(|v| v.as_str()).unwrap_or("?").to_string();
                let sval = m
                    .get("status")
                    .and_then(|s| s.get("value"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("?")
                    .to_string();
                (mid, sval)
            })
            .collect();
        // A single llama.cpp lists its model WITHOUT status fields — the
        // presence of a sampler row IS the loaded truth (the server IS its
        // model; there is no unloaded state to paint).
        let is_loaded = |id: &str, s: &str| {
            s == "loaded" || (s == "?" && rows.iter().any(|r| r.model == id))
        };
        let is_live = |id: &str, s: &str| {
            is_loaded(id, s) || s == "loading" || s == "unloading"
        };
        let live: Vec<&String> = lanes
            .iter()
            .filter(|(id, s)| is_live(id, s))
            .map(|(id, _)| id)
            .collect();
        loaded_n = lanes
            .iter()
            .filter(|(id, s)| is_loaded(id, s))
            .count();
        total_n = lanes.len();
        for (mid, sval) in &lanes {
            if (sval == "loaded" || sval == "loading" || sval == "unloading")
                && !alive.contains(mid)
            {
                alive.push(mid.clone());
            }
        }
        if !live.is_empty() {
            // eviction: the box's memory is the CURRENT run, not a museum
            let live_ids: Vec<&str> = live.iter().map(|s| s.as_str()).collect();
            alive.retain(|g| live_ids.contains(&g.as_str()));
        }
        // Pass 2: paint. Live lanes always ride (swapping keeps its name,
        // status word parks in ACCEPT). Cold alive lanes ride only while
        // nothing is live; show_all rides every listed lane. The focused
        // lane is PINNED to the top row (pin_focus_lanes law).
        for &li in &pin_focus_lanes(&lanes, focus) {
            let (mid, sval) = (&lanes[li].0, &lanes[li].1);
            let loaded = is_live(mid, sval);
            let swapping = sval == "loading" || sval == "unloading";
            let ghost = !loaded && !swapping && alive.contains(mid);
            if !loaded && !swapping && !show_all && !ghost {
                continue;
            }
            match rows.iter().find(|r| r.model == *mid) {
                Some(r) => out.push(DashRow { cells: table_cells(r, jt) }),
                None => out.push(DashRow {
                    cells: [
                        mid.clone(),
                        "\u{2014}".into(),
                        "\u{2014}".into(),
                        "\u{2014}".into(), // TOTAL: unloaded has no live counter
                        "\u{2014}".into(),
                        "\u{2014}".into(),
                        "\u{2014}".into(),
                        "\u{2014}".into(),
                        sval.clone(),
                    ],
                }),
            }
        }
        // ghosts that VANISHED from this snapshot (a direct llama.cpp router
        // stops listing what it unloaded): they hold the idle box open —
        // and only an idle box (R9).
        if live.is_empty() {
            for g in alive.iter() {
                if lanes.iter().any(|(id, _)| id == g) {
                    continue;
                }
                out.push(DashRow {
                    cells: [
                        g.clone(),
                        "\u{2014}".into(),
                        "\u{2014}".into(),
                        "\u{2014}".into(),
                        "\u{2014}".into(),
                        "\u{2014}".into(),
                        "\u{2014}".into(),
                        "\u{2014}".into(),
                        "idle".into(),
                    ],
                });
            }
        }
    } else {
        // no models endpoint: every sampler row is a live lane
        alive.clear();
        for r in rows {
            total_n += 1;
            loaded_n += 1;
            alive.push(r.model.clone());
            out.push(DashRow { cells: table_cells(r, jt) });
        }
    }
    // Nothing alive and nothing loaded — the ONE case that reads `no model`
    // (in the first column, a real row; never a reserved blank line).
    if out.is_empty() {
        out.push(DashRow {
            cells: [
                "no model".into(),
                "\u{2014}".into(),
                "\u{2014}".into(),
                "\u{2014}".into(),
                "\u{2014}".into(),
                "\u{2014}".into(),
                "\u{2014}".into(),
                "\u{2014}".into(),
                "\u{2014}".into(),
            ],
        });
    }
    (out, loaded_n, total_n)
}

fn legend<'a>(title: &'a str, mut parts: Vec<Part<'a>>) -> Legend<'a> {
    // the parts ARE the label-row truth (KV drops avg/max on purpose)
    let _ = &mut parts;
    Legend { title, parts }
}

/// avg/max pair for the throughput graphs (tight, no `now`).
fn avg_max(ring: &lsmtop::hist::Ring) -> Vec<Part<'static>> {
    let (avg, max) = ring_stats(ring);
    vec![Part::Num("avg", avg), Part::Num("max", max)]
}

/// GEN label extras (maintainer 2026-11-07): current n_gen as % of the real
/// output room = ctx minus this task's prompt. Journal truth only.
fn gen_pct_of(ctx_k: Option<u64>, jt: Option<lsmtop::sampling::JournalTotal>) -> Option<f64> {
    let j = jt.filter(|j| j.decoding && j.gen.unwrap_or(0.0) > 0.0)?;
    let ctx = ctx_k? as f64 * 1024.0;
    let room = ctx - j.prompt_n.unwrap_or(0.0);
    if room > 0.0 {
        Some((j.gen? / room * 100.0).min(100.0))
    } else {
        None
    }
}

/// Header parity: full-width accent bar, page title centered, live clock
/// right. ONE title bar for the whole app (maintainer 2026-11-07): every screen
/// names itself here — " Settings ", " Journal " — never a second title
/// inside the page or a box title on the journal.
fn draw_header(buf: &mut ratatui::buffer::Buffer, area: Rect, title: &str) {
    let bar = style::header();
    for x in area.x..area.x + area.width {
        buf[(x, area.y)].set_char(' ').set_style(bar);
    }
    if area.width >= 12 {
        buf.set_string(area.x, area.y, " \u{2b58}", bar);
        let tx = area.x + (area.width.saturating_sub(title.chars().count() as u16)) / 2;
        buf.set_string(tx, area.y, title, bar);
        let clock = chrono::Local::now().format("%H:%M:%S").to_string();
        if area.width > clock.chars().count() as u16 + 6 {
            buf.set_string(
                area.x + area.width - clock.chars().count() as u16 - 1,
                area.y,
                &clock,
                bar,
            );
        }
    }
}

/// Settings label column law: 16 wide ("tick rate 2000ms" is exactly 16),
/// arrow at 0, a space, then buttons/chips start at column 18. The spans
/// AND the hit rects below feed these constants — the mirror trap.
const LBL_W: usize = 16;
const CHIP_COL: u16 = 18;

fn render(rt: &mut Rt, f: &mut ratatui::Frame) {
    rt.hit.clear(); // rebuilt by the draws below (last-frame hit map)
    let full = f.area();
    // ONE title bar: the metrics screen shows the live banner; every other
    // screen just names itself in the same blue/white chrome.
    let head_title: String = if matches!(rt.top(), Screen::Metrics) && full.height >= 5 {
        let t = rt.target();
        let name = t
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("router")
            .to_string();
        let st = rt.ts();
        let (_, loaded_n, total_n) =
            build_table_rows(st.last_models.as_ref(), &st.sampler.rows, true, rt.jtot.as_ref(), &mut Vec::new(), st.sampler.focus.as_deref());
        let uinfo = rt.unit_infos.get(&rt.target_ix).and_then(|r| r.as_ref().ok());
        let u = uinfo.map(|ui| (ui.active_state.as_str(), ui.main_pid));
        // tightened per the 2026-11-07 pass: the bare count, not "1/11 lanes loaded"
        format!(
            " {name} \u{00b7} {loaded_n}/{total_n} \u{00b7} {} ",
            banner_detail(u, st.mode)
        )
    } else {
        match rt.top() {
            Screen::Settings => " Settings ".to_string(),
            Screen::Logs => " Journal ".to_string(),
            Screen::Metrics => " lsmtop ".to_string(),
        }
    };
    let body = if full.height >= 5 {
        draw_header(
            f.buffer_mut(),
            Rect { x: full.x, y: full.y, width: full.width, height: 1 },
            &head_title,
        );
        Rect { x: full.x, y: full.y + 1, width: full.width, height: full.height - 1 }
    } else {
        full
    };
    match rt.top() {
        Screen::Metrics => {
            // v9.15 shell: stats · 4 legend graphs · 8-col table ·
            // hotkeys (the banner rides the header bar now).
            let gopts = graph_opts_from(&rt.opts);
            let vis = [
                rt.opts.value("gen_graph") == "on",
                rt.opts.value("prompt_graph") == "on",
                rt.opts.value("kv_graph") == "on",
                rt.opts.value("gpu_graph") == "on",
            ];
            let sizes = {
                let mv = rt.config.metrics();
                [
                    PaneSize::from_mode(&mv.gen_size),
                    PaneSize::from_mode(&mv.prompt_size),
                    PaneSize::from_mode(&mv.kv_size),
                    PaneSize::from_mode(&mv.gpu_size),
                ]
            };
            // lanes TABLE mode (L cycles, chip `lanes` edits): on = active
            // only, all = every model, off = the pane is GONE (graphs take
            // the space; the shared graph_layout law gets show=false).
            let lmode = rt.config.metrics().lanes;
            let show_lanes = lmode != "off";
            let ti = rt.target_ix.min(rt.states.len() - 1);
            let st = &mut rt.states[ti];
            let (mut rows_dash, _loaded_n, _total_n) =
                build_table_rows(st.last_models.as_ref(), &st.sampler.rows, lmode == "all", rt.jtot.as_ref(), &mut st.lane_alive, st.sampler.focus.as_deref());
            if !show_lanes {
                rows_dash.clear();
            }
            let rows_len_win = if show_lanes { rows_dash.len() } else { 0 };
            let (used, total, avail) = lsmtop::gpu::mem_gib();
            let stats_base = if vis[3] {
                let gtxt = rt
                    .ts()
                    .sampler
                    .gpu_busy
                    .map(|g| format!("{g:3.0}%"))
                    .unwrap_or_else(|| " n/a ".to_string());
                match lsmtop::gpu::vram_gib() {
                    Some((vu, vt)) => format!("GPU {gtxt} \u{00b7} VRAM {vu:.1}/{vt:.1} GiB"),
                    None => format!(
                        "APU {gtxt} \u{00b7} RAM {used:.1}/{total:.0} GiB used, {avail:.0} avail (unified APU)"
                    ),
                }
            } else {
                format!(
                    "RAM {used:.1}/{total:.0} GiB used, {avail:.0} avail"
                )
            };
            let dev = lsmtop::gpu::device_labels().into_iter().next().unwrap_or_default();
            let stats = if dev.is_empty() {
                stats_base
            } else {
                format!("{stats_base} \u{00b7} {dev}")
            };
            // G2: the strip is the primary UI — accented letters, each a
            // mouse target, no focus mode anywhere.
            let strip_toks: Vec<StripTok> = metrics_strip();
            let (stext, strip_cols) = strip_layout(&strip_toks);
            // B4/G1: pane size law against the live body; the journal
            // flexes before graphs lose their per-pane minimums. Tiers are
            // FIXED row counts (four/ten) — nothing about the newest line
            // may change the dashboard's flow anymore.
            let jmode = rt.config.metrics().journal;
            let jh = journal_pane_h(body.height, &jmode, graphs_min(&vis, &sizes));
            let journal = if jh > 0 {
                rt.jpane.as_ref().map(|ls| JournalPane {
                    lines: &ls.lines,
                    scroll: ls.scroll,
                })
            } else {
                None
            };
            {
                let ti = rt.target_ix.min(rt.states.len() - 1);
                let s = &mut rt.states[ti].sampler;
                // label-row extras (maintainer 2026-11-07 law): no 'now' — the
                // value rides the right edge; GEN gains gen%, KV gains
                // hit% + proc%, the GPU line carries its electrical number
                let jt = rt.jtot;
                let lane = s.rows.iter().find(|r| r.active).or_else(|| s.rows.first());
                let hit = lane.and_then(|l| l.cache_pct);
                let proc = jt.filter(|j| !j.decoding).and_then(|j| j.prompt_pct);
                let genp = gen_pct_of(lane.and_then(|l| l.ctx_k), jt);
                let clk = s.gpu_clk; // sampled per tick, never per frame (nvidia-smi costs a fork)
                let pwr = s.gpu_pwr;
                let w = DashboardWidget {
            seam: s.seam_back,
                    stats: &stats,
                    hotkeys: &stext,
                    hotkey_toks: &strip_toks,
                    opts: gopts,
                    tick: s.polls,
                    tick_ms: rt.config.tick_ms() as u64,
                    gen: &s.gen,
                    gen_leaky: &mut s.gen_leaky,
                    gen_legend: legend(
                        "GEN t/s",
                        [
                            avg_max(&s.gen),
                            genp.map(|p| vec![Part::Pct("gen", p)]).unwrap_or_default(),
                        ]
                        .concat(),
                    ),
                    prompt: &s.prompt,
                    prompt_leaky: &mut s.prompt_leaky,
                    prompt_legend: legend("PROMPT t/s", avg_max(&s.prompt)),
                    kv: &s.kv,
                    kv_legend: legend(
                        "KV\u{2248} %",
                        [
                            hit.map(|h| Part::Pct("hit", h)),
                            proc.map(|pr| Part::Pct("proc", pr * 100.0)),
                        ]
                        .into_iter()
                        .flatten()
                        .collect(),
                    ),
                    apu: &s.apu,
                    apu_legend: legend(
                                                apu_title(),
                        [
                            clk.map(|c| Part::Raw("clk", format!("{c:.0}MHz"))),
                            pwr.map(|w| Part::Raw("pwr", format!("{w:.0}W"))),
                        ]
                        .into_iter()
                        .flatten()
                        .collect(),
                    ),
                    vis,
                    sizes,
                    graph_scroll: rt.graph_scroll,
                    show_lanes,
                    journal,
                    journal_h: jh,
                    rows: rows_dash,
                    scale: lsmtop::ui::dashboard::ScalePos::parse(&rt.config.metrics().scale),
                };
                f.render_widget(w, body);
            }
            // B2/G1: snapshot the graphs window for wheel scrolling — the
            // SAME shared geometry functions the widget uses (dash_avail_h
            // / graphs_rect / journal_pane_rect); drift = wheel pans the
            // wrong band (the B4 mirror trap).
            {
                let avail = dash_avail_h(body.height, jh);
                let (content, _table_h, gh) =
                    graph_layout(avail, rows_len_win as u16, graphs_min(&vis, &sizes), show_lanes);
                rt.jpane_win = journal_pane_rect(body, jh);
                rt.graphs_win = Some((graphs_rect(body, gh), content.saturating_sub(gh)));
            }
            // G2: every strip WORD is a mouse target (btop style: the
            // hotkey letter is inside the word); columns come from
            // strip_layout — the same source that painted the strip.
            {
                let row = body.y + body.height.saturating_sub(1);
                for (t, (start, _hot, key)) in strip_toks.iter().zip(&strip_cols) {
                    rt.hit.push((
                        Rect::new(body.x + start, row, t.label.chars().count() as u16, 1),
                        Hit::Key(KeyCode::Char(*key)),
                    ));
                }
            }
        }
        Screen::Settings => {
            // Options screen, 2026-11-07 law: no text fields left (the
            // server/HF settings left with their features). The ring walks
            // the TICK row + every option chip row, one BLANK line between
            // rows, all buttons share the blue/white button doctrine.
            let n_opts = rt.opts.rows.len();
            let n_rows = 1 + n_opts; // ring rows: tick + options
            let se0 = rt.setst.clone().unwrap_or_default();
            let vis = body.height.saturating_sub(1); // footer row excluded
            let span = set_focus_rows(se0.focus);
            let content_len = (2 + 2 * n_rows) as u16;
            let scroll = follow_scroll(se0.scroll, span, vis.max(1), content_len);
            if let Some(se) = rt.setst.as_mut() {
                se.scroll = scroll;
            }
            let se = rt.setst.clone().unwrap_or_default();
            let mut lines: Vec<Line> = Vec::new();
            lines.push(Line::styled(
                "  Metrics".to_string(),
                Style::default().add_modifier(Modifier::BOLD),
            ));
            lines.push(Line::from(""));
            let mut opt_rows: Vec<u16> = Vec::new();

            // ROW 0 of the ring: Tick rate — a number field the keys never
            // type into; `ms` is painted with the value, and the two small
            // buttons do the stepping.
            {
                let focused = se.focus == 0;
                let lstyle = if focused { style::focus_label() } else { Style::default() };
                let ms = rt.config.tick_ms();
                let mut spans = vec![
                    Span::styled(if focused { "\u{25b8}" } else { " " }, lstyle),
                    Span::styled(format!("{:>1$}", "tick rate", LBL_W), lstyle),
                    Span::styled(" ", Style::default()),
                ];
                for (i, (txt, st)) in [
                    (
                        "[ + ]".to_string(),
                        style::plus(focused && se.tick_sel == 0),
                    ),
                    (
                        "[ - ]".to_string(),
                        style::minus(focused && se.tick_sel == 1),
                    ),
                ]
                .iter()
                .enumerate()
                {
                    if i > 0 {
                        spans.push(Span::styled(" ", Style::default()));
                    }
                    spans.push(Span::styled(txt.clone(), *st));
                }
                // value rides to the RIGHT of the buttons (maintainer
                // 2026-11-07): the row reads  tick rate [ + ] [ - ] 1000ms
                // T2: tick-rate value rides plain at all times — no focus-style
                // highlight on the ms suffix; only buttons wear the row-color.
                spans.push(Span::styled(format!("   {ms}ms"), Style::default()));
                opt_rows.push(lines.len() as u16);
                lines.push(Line::from(spans));
                lines.push(Line::from(""));
            }
            for (i, row) in rt.opts.rows.iter().enumerate() {
                let focused = se.focus == 1 + i;
                let lstyle = if focused { style::focus_label() } else { Style::default() };
                let base = if row.disabled {
                    style::button_disabled()
                } else {
                    Style::default()
                };
                let mut spans = vec![
                    Span::styled(if focused { "\u{25b8}" } else { " " }, lstyle),
                    Span::styled(format!("{:>1$}", row.key, LBL_W), lstyle),
                    Span::styled(" ", base),
                ];
                for (j, v) in row.values.iter().enumerate() {
                    // button doctrine: EVERY bracketed value is lit (white on
                    // muted blue); the active one is the title-bar blue/white
                    // combo (REVERSED = visible everywhere, harness included)
                    let st = if j == row.active && !row.disabled {
                        style::button_active()
                    } else {
                        style::button().patch(base)
                    };
                    spans.push(Span::styled(format!("[ {} ]", v), st));
                    if j + 1 < row.values.len() {
                        spans.push(Span::styled(" ", base));
                    }
                }
                opt_rows.push(lines.len() as u16);
                lines.push(Line::from(spans));
                lines.push(Line::from("")); // one space line between options
            }
            f.render_widget(
                Paragraph::new(lines).scroll((scroll, 0)),
                Rect { x: body.x, y: body.y, width: body.width, height: vis },
            );
            // mouse hits — content rows shifted UP by the scroll, clipped to
            // the visible window (a rect above the fold must not eat clicks)
            let win = |cy: u16| -> Option<u16> {
                if cy < scroll || cy >= scroll + vis {
                    None
                } else {
                    Some(body.y + cy - scroll)
                }
            };
            // Row-focus rects go FIRST; chip/button rects are pushed after
            // and win the rposition() match in hit_at.
            for (ix, ry) in opt_rows.iter().enumerate() {
                if let Some(y) = win(*ry) {
                    rt.hit.push((
                        Rect { x: body.x, y, width: body.width, height: 1 },
                        Hit::RowFocus(ix),
                    ));
                }
            }
            // tick row buttons at CHIP_COL (the mirror trap: paint and hit
            // come from the same constants)
            if let Some(y) = win(opt_rows[0]) {
                for (i, dir) in [(0usize, 1isize), (1, -1)].iter() {
                    let x = body.x + CHIP_COL + (*i as u16) * 6;
                    rt.hit.push((Rect { x, y, width: 5, height: 1 }, Hit::TickBtn(*dir)));
                }
            }
            for (i, row) in rt.opts.rows.iter().enumerate() {
                let ry = match win(opt_rows[1 + i]) {
                    Some(y) => y,
                    None => continue,
                };
                let mut cx = body.x + CHIP_COL;
                for (j, v) in row.values.iter().enumerate() {
                    let lw = v.chars().count() as u16 + 4;
                    if cx + lw > body.x + body.width {
                        break;
                    }
                    rt.hit.push((
                        Rect { x: cx, y: ry, width: lw, height: 1 },
                        Hit::OptsChip(i, j),
                    ));
                    cx += lw + 1;
                }
            }
            // footer via the shared strip: back is esc AND q; the chips and
            // the tick value save the moment they change — no save talk
            let foot = [
                FootTok { hot: "esc/q", label: " Back", key: Some(KeyCode::Esc) },
                FootTok {
                    hot: "\u{2191}\u{2193}",
                    label: " Select",
                    key: Some(KeyCode::Down),
                },
                FootTok { hot: "enter", label: " Pick", key: Some(KeyCode::Enter) },
            ];
            let hits =
                strip::draw_foot(f.buffer_mut(), body.x, body.y + vis, body.width, &foot);
            rt.hit.extend(hits.into_iter().map(|(r, k)| (r, Hit::Key(k))));
        }
        Screen::Logs => {
            // Full-screen journal: bare rows (no box, no second Clear hint
            // — the title bar and the bottom strip carry the chrome).
            if let Some(ls) = rt.logs.as_ref() {
                // R2: the back hint lives in the bottom strip, left-most,
                // like every other screen.
                let logs_area = Rect { height: body.height.saturating_sub(1), ..body };
                f.render_widget(LogsWidget(ls), logs_area);
            }
            // hot strip: j CLOSES the full screen (cycle continues, maintainer:
            // "esc Back is no longer needed"), so j rides the hot glyphs;
            // the click dispatches Esc, its equivalent.
            let toks = [
                FootTok { hot: "esc/q/j", label: " Back", key: Some(KeyCode::Esc) },
                FootTok { hot: "c", label: " Clear", key: Some(KeyCode::Char('c')) },
            ];
            let fy = body.y + body.height.saturating_sub(1);
            let hits = strip::draw_foot(f.buffer_mut(), body.x, fy, body.width, &toks);
            rt.hit.extend(hits.into_iter().map(|(r, k)| (r, Hit::Key(k))));
        }
    }
}

#[cfg(test)]
mod shell_tests {
    //! Settings is hotkey-first with NO text fields; the hit map must
    //! resolve to the widget the pixels show; the tick repeat gate must
    //! behave like a hold (one step on tap, delayed glide on hold).
    use super::*;

    fn sk(k: SetKey) -> SetKey {
        k
    }

    #[test]
    fn route_covers_ring_activate_and_dir() {
        // tick row (0): enter fires the highlighted button, arrows slide it
        assert_eq!(route_settings(&sk(SetKey::Activate), 0), Route::TickPress);
        assert_eq!(route_settings(&sk(SetKey::ChipDir(1)), 0), Route::TickSel(1));
        assert_eq!(route_settings(&sk(SetKey::ChipDir(-1)), 0), Route::TickSel(-1));
        // option rows (1+): enter cycles forward, arrows cycle by one
        assert_eq!(route_settings(&sk(SetKey::Activate), 1), Route::CycleChip(1));
        assert_eq!(route_settings(&sk(SetKey::ChipDir(-1)), 15), Route::CycleChip(-1));
        // ring + q
        assert_eq!(route_settings(&sk(SetKey::NextField), 0), Route::Move(1));
        assert_eq!(route_settings(&sk(SetKey::PrevField), 0), Route::Move(-1));
        assert_eq!(route_settings(&sk(SetKey::Char('q')), 0), Route::EscBack);
        assert_eq!(route_settings(&sk(SetKey::Char('m')), 0), Route::Ignore);
    }

    #[test]
    fn settings_row_law_has_a_blank_between_rows() {
        // row f lives at 2+2f: every option row is followed by a blank
        assert_eq!(set_focus_rows(0), (2, 2));
        assert_eq!(set_focus_rows(1), (4, 4));
        assert_eq!(set_focus_rows(14), (30, 30));
    }

    #[test]
    fn settings_scroll_follow_and_clamp() {
        // focus below the window pulls the scroll down (minimal: line 30
        // rides the LAST row of a 10-row window -> sc 21); above pulls it up
        assert_eq!(follow_scroll(0, (30, 30), 10, 32), 21);
        assert_eq!(follow_scroll(22, (30, 30), 10, 32), 22);
        assert_eq!(follow_scroll(12, (2, 2), 10, 32), 2);
        // clamp: never scroll past the content bottom
        assert_eq!(clamp_scroll(99, 32, 10), 22);
        assert_eq!(clamp_scroll(5, 32, 40), 0); // content fits -> no scroll
    }

    #[test]
    fn up_down_ring_includes_the_tick_row_and_every_option() {
        // ring math is applied in the handler as
        // (focus + d).rem_euclid(1 + n_opts)
        let n = (1 + lsmtop::ui::opts::ROW_KEYS.len()) as isize;
        let mut focus = 0usize;
        for _ in 0..n {
            focus = ((focus as isize - 1).rem_euclid(n)) as usize;
        }
        assert_eq!(focus, 0);
        assert_eq!(((n - 1 + 1).rem_euclid(n)) as usize, 0); // last row wraps back
    }

    #[test]
    fn hit_map_resolves_inside_only() {
        let r = Rect { x: 2, y: 3, width: 10, height: 1 };
        let hits = vec![(r, Hit::RowFocus(0))];
        assert_eq!(hit_at(&hits, 2, 3), Some(0));
        assert_eq!(hit_at(&hits, 11, 3), Some(0));
        assert_eq!(hit_at(&hits, 12, 3), None);
        assert_eq!(hit_at(&hits, 2, 4), None);
    }

    #[test]
    fn tick_gate_taps_apply_holds_glide() {
        // a single tap: applies immediately
        let mut g = None;
        assert!(step_gate_allow(1000, &mut g, 1));
        // terminal repeats right away: swallowed (fine tuning = one step)
        for t in [1033, 1066, 1099, 1132] {
            assert!(!step_gate_allow(t, &mut g, 1), "repeat at {t} must be swallowed");
        }
        // hold passes the 500 ms delay: repeats start, throttled to one/GAP
        assert!(step_gate_allow(1501, &mut g, 1), "delay elapsed -> apply");
        assert!(!step_gate_allow(1534, &mut g, 1), "inside the GAP throttle");
        assert!(step_gate_allow(1621, &mut g, 1), "past the GAP -> glide");
        // release silence > COALESCE, then a fresh tap applies at once
        assert!(step_gate_allow(2300, &mut g, 1));
        // direction flip always re-arms the first-press rule
        assert!(step_gate_allow(2305, &mut g, -1));
    }

    #[test]
    fn tick_bounds_are_the_grid_ends() {
        // 500..5000 in 50 steps: bound presses are no-ops (None), not wraps
        assert_eq!(tick_step_from(500, -1), None);
        assert_eq!(tick_step_from(500, 1), Some(550));
        assert_eq!(tick_step_from(5000, 1), None);
        assert_eq!(tick_step_from(5000, -1), Some(4950));
        assert_eq!(lsmtop::config::normalize_tick_ms(123), 500);
        assert_eq!(lsmtop::config::normalize_tick_ms(99999), 5000);
        assert_eq!(lsmtop::config::normalize_tick_ms(2037), 2050);
        assert_eq!(lsmtop::config::normalize_tick_ms(2024), 2000);
    }
}

// ANTI-FIXTURE GATE (maintainer 2026-11-09): the dashboard snapshots hand-build
// DashRow cell strings and therefore prove geometry only. THESE asserts are
// the only proof that the REAL table_cells() output carries the 2026-11-09
// unit law (units ride PROMPT/GEN/CACHE cells; falsy -> em dash).
#[cfg(test)]
mod cell_format_tests {
    use super::table_cells;
    use lsmtop::sampling::{JournalTotal, LaneRow};

    fn row() -> LaneRow {
        LaneRow {
            port: 8081,
            model: "gemma".into(),
            gen_ts: 45.23,
            prompt_ts: 610.4,
            cache_pct: Some(82.0),
            kv: 33.0,
            active: true,
            act: "1".into(),
            ctx_k: Some(128),
            acc: Some(71.0),
            total_tk: Some(812.0),
        }
    }

    #[test]
    fn live_lane_cells_carry_units() {
        // A live run's slot counters ARE the running total — no journal,
        // no blank (maintainer 2026-11-10: TOTAL never renders empty while the
        // lane counts tokens).
        assert_eq!(
            table_cells(&row(), None),
            ["gemma", "1", "610t/s", "812", "45.2t/s", "33.0%", "82%", "128k", "71%"]
        );
    }

    #[test]
    fn prompt_tier_prints_kt_s() {
        let mut r = row();
        r.prompt_ts = 1240.0;
        assert_eq!(table_cells(&r, None)[2], "1.2kt/s");
    }

    #[test]
    fn idle_lane_is_all_dashes_except_zero_cache() {
        let mut r = row();
        r.gen_ts = 0.0;
        r.prompt_ts = 0.0;
        r.cache_pct = None;
        r.kv = 0.0;
        r.ctx_k = None;
        r.acc = None;
        r.total_tk = None; // nothing live/cold never counts (B1 blank law)
        assert_eq!(
            table_cells(&r, None),
            ["gemma", "1", "\u{2014}", "\u{2014}", "\u{2014}", "0.0%", "\u{2014}", "\u{2014}", "\u{2014}"]
        );
    }

    #[test]
    fn journal_total_rides_between_prompt_and_gen() {
        let jt = JournalTotal { decoding: true, gen: Some(812.0), prompt_n: None, prompt_pct: None };
        assert_eq!(table_cells(&row(), Some(&jt))[3], "812");
        let jt = JournalTotal {
            decoding: false,
            gen: None,
            prompt_n: Some(2048.0),
            prompt_pct: Some(0.5),
        };
        assert_eq!(table_cells(&row(), Some(&jt))[3], "2048 50%");
    }
}

// Task 4 (maintainer 2026-11-09): the LANES bar reserves a status line —
// R8 LANE-ALIVE LAW (supersedes R7's reserved status line): a lane that
// was loaded once keeps a ghost row after unload so the LANES box never
// resizes; `no model` rides the first column only when nothing is known.
#[cfg(test)]
mod lane_alive_tests {
    use super::{build_table_rows, pin_focus_lanes};
    use serde_json::json;

    #[test]
    fn a_loaded_lane_never_leaves_the_box() {
        let mut alive = Vec::new();
        let loaded = json!({"data":[{"id":"a","status":{"value":"loaded"}}]});
        let (rows, l, t) = build_table_rows(Some(&loaded), &[], false, None, &mut alive, None);
        assert_eq!(rows.len(), 1);
        assert_eq!((l, t), (1, 1));
        // the model unloads AND vanishes from the listing (direct llama.cpp
        // router): the ghost row keeps the box the same size (R8, maintainer)
        let gone = json!({"data":[]});
        let (rows, l, t) = build_table_rows(Some(&gone), &[], false, None, &mut alive, None);
        assert_eq!(rows.len(), 1, "ghost row keeps the box size");
        assert_eq!(rows[0].cells[0], "a");
        assert_eq!(rows[0].cells[8], "idle");
        assert_eq!((l, t), (0, 0));
        // show_all must not paint the ghost twice
        let (rows, _, _) = build_table_rows(Some(&gone), &[], true, None, &mut alive, None);
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn listed_ghost_rides_the_engines_own_word() {
        let mut alive = Vec::new();
        let loaded = json!({"data":[{"id":"a","status":{"value":"loaded"}}]});
        build_table_rows(Some(&loaded), &[], false, None, &mut alive, None);
        // llama-swap keeps listing the model, now cold: the alive lane stays
        // in the normal view carrying the ENGINE's word, not a spare line
        let unload = json!({"data":[{"id":"a","status":{"value":"unloaded"}}]});
        let (rows, l, t) = build_table_rows(Some(&unload), &[], false, None, &mut alive, None);
        assert_eq!(rows.len(), 1, "alive lane stays even though cold");
        assert_eq!(rows[0].cells[0], "a");
        assert_eq!(rows[0].cells[8], "unloaded");
        assert_eq!((l, t), (0, 1));
    }

    #[test]
    fn loading_lane_keeps_name_with_status_in_accept() {
        let mut alive = Vec::new();
        let loading = json!({"data":[{"id":"a","status":{"value":"loading"}}]});
        let (rows, l, _t) = build_table_rows(Some(&loading), &[], false, None, &mut alive, None);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].cells[0], "a");
        assert_eq!(rows[0].cells[8], "loading");
        assert_eq!(l, 0);
    }

    // R9 (maintainer 2026-11-10, live router test): a ghost row exists ONLY
    // while NOTHING is loaded — it holds the box open at idle. The moment a
    // live lane appears, ghosts are EVICTED for good, not just hidden.
    
    #[test]
    fn focused_lane_pins_the_top_row() {
        // pin law (maintainer 2026-11-10): the curve's lane rides the TOP row —
        // its `loaded` word there IS the focus marker; no focus, no move.
        let lanes = vec![
            ("a".to_string(), "loaded".to_string()),
            ("b".to_string(), "loaded".to_string()),
            ("c".to_string(), "unloaded".to_string()),
        ];
        assert_eq!(pin_focus_lanes(&lanes, Some("b")), vec![1, 0, 2]);
        assert_eq!(pin_focus_lanes(&lanes, None), vec![0, 1, 2]);
        assert_eq!(pin_focus_lanes(&lanes, Some("zz")), vec![0, 1, 2]);
        // paint proof: the focused lane is top even when listed second
        let both = json!({"data":[
            {"id":"a","status":{"value":"loaded"}},
            {"id":"b","status":{"value":"loaded"}}]});
        let (rows, _l, _t) =
            build_table_rows(Some(&both), &[], false, None, &mut Vec::new(), Some("b"));
        assert_eq!(rows[0].cells[0], "b", "the curve's lane owns the top row");
        assert_eq!(rows[1].cells[0], "a");
    }

    #[test]
    fn ghost_evicts_when_another_model_loads() {
        let mut alive = Vec::new();
        let a = json!({"data":[{"id":"a","status":{"value":"loaded"}}]});
        build_table_rows(Some(&a), &[], false, None, &mut alive, None);
        // A goes cold, B loads (llama-swap lists both): only B rides the
        // table — one row, not live + persistent ghost.
        let both = json!({"data":[
            {"id":"a","status":{"value":"unloaded"}},
            {"id":"b","status":{"value":"loaded"}}]});
        let (rows, l, t) = build_table_rows(Some(&both), &[], false, None, &mut alive, None);
        assert_eq!(rows.len(), 1, "live lane leaves no room for ghosts");
        assert_eq!(rows[0].cells[0], "b");
        assert_eq!((l, t), (1, 2));
        assert_eq!(alive.as_slice(), &["b".to_string()]);
        // a VANISHED ghost (direct router stopped listing it) is evicted too
        let mut alive2 = vec!["a".to_string()];
        let b_only = json!({"data":[{"id":"b","status":{"value":"loaded"}}]});
        let (rows, _, _) = build_table_rows(Some(&b_only), &[], false, None, &mut alive2, None);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].cells[0], "b");
        assert_eq!(alive2.as_slice(), &["b".to_string()]);
        // when B later unloads, only B's memory holds the box — A is gone
        let b_cold = json!({"data":[{"id":"b","status":{"value":"unloaded"}}]});
        let (rows, _, _) = build_table_rows(Some(&b_cold), &[], false, None, &mut alive2, None);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].cells[0], "b");
    }

    #[test]
    fn swapping_lane_is_live_so_ghosts_evict() {
        let mut alive = vec!["a".to_string()];
        // B mid-swap is real activity: A's ghost is evicted, B keeps its
        // name with the status word (Task-4 swap law survives R9)
        let swapping = json!({"data":[
            {"id":"a","status":{"value":"unloaded"}},
            {"id":"b","status":{"value":"loading"}}]});
        let (rows, _, _) = build_table_rows(Some(&swapping), &[], false, None, &mut alive, None);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].cells[0], "b");
        assert_eq!(rows[0].cells[8], "loading");
        assert_eq!(alive.as_slice(), &["b".to_string()]);
    }

    #[test]
    fn nothing_known_reads_no_model_in_first_column() {
        let mut alive = Vec::new();
        let empty = json!({"data":[]});
        let (rows, _, _) = build_table_rows(Some(&empty), &[], false, None, &mut alive, None);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].cells[0], "no model");
        // a cold never-loaded catalogue is NOT a known lane: still no model
        let cold = json!({"data":[{"id":"z","status":{"value":"unloaded"}}]});
        let (rows, _, _) = build_table_rows(Some(&cold), &[], false, None, &mut alive, None);
        assert_eq!(rows[0].cells[0], "no model");
    }

    // Phase 1 (single llama.cpp): a bare instance lists its model WITHOUT
    // status fields; the sampler row IS the loaded truth — the table must
    // never read `no model` beside a live single-instance row.
    fn one_row(model: &str) -> lsmtop::sampling::LaneRow {
        lsmtop::sampling::LaneRow {
            port: 0,
            model: model.into(),
            gen_ts: 0.0,
            prompt_ts: 0.0,
            cache_pct: None,
            kv: 0.0,
            active: true,
            act: "0".into(),
            ctx_k: None,
            acc: None,
            total_tk: None,
        }
    }

    #[test]
    fn single_instance_listing_with_live_row_counts_loaded() {
        let mut alive = Vec::new();
        let models = json!({"object":"list","data":[
            {"id":"gemma-3-12b","object":"model"}]});
        let r = one_row("gemma-3-12b");
        let (rows, l, t) = build_table_rows(Some(&models), &[r], false, None, &mut alive, None);
        assert_eq!(rows.len(), 1, "status-less listing paints its row");
        assert_eq!(rows[0].cells[0], "gemma-3-12b");
        assert_eq!((l, t), (1, 1), "banner reads 1/1, not 0/1");
        // a status-less entry WITHOUT a sampler row is not evidence:
        let (rows, l, _) = build_table_rows(Some(&models), &[], false, None, &mut alive, None);
        assert_eq!(rows[0].cells[0], "no model");
        assert_eq!(l, 0);
    }
}
