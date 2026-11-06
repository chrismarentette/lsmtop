//! lsmtop — top-style live monitor for llama.cpp / llama-swap (Rust).
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
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers,
        MouseButton, MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Terminal,
};

use lsmtop::app::{Action, App, AppEvent, SampleData, Screen, SetKey};
use lsmtop::config::Config;
use lsmtop::net::collector;
use lsmtop::paint::glyphs::{Chars, Fill};
use lsmtop::paint::modes::Mode;
use lsmtop::paint::ramps::Ramp;
use lsmtop::sampling::Sampler;
use lsmtop::svc::logs::journal_cmd;
use lsmtop::svc::owned::OwnedProcess;
use lsmtop::svc::{systemctl_show, CmdRunner, RealRunner, UnitInfo};
use lsmtop::hf;
use lsmtop::ui::hf::{HfOut, HfState, HfWidget};
use lsmtop::ui::dashboard::{journal_single_rows, 
    dash_avail_h, graph_layout, graphs_min, graphs_rect, journal_pane_h, journal_pane_rect,
    metrics_strip, ring_stats, strip_layout, strip_text, DashRow, DashboardWidget, JournalPane,
    Legend, PaneSize, StripTok,
};
use lsmtop::ui::graph_widget::GraphOpts;
use lsmtop::ui::logs::{LogSource, LogState, LogsWidget};
use lsmtop::ui::manager::{
    footer_text, plan_action, router_lane_lines, ActionPlan, ManagerWidget, PanelInfo,
};
use lsmtop::ui::opts::{OptsState, OptsWidget, OPTS_FOOTER};

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn logs_dir() -> std::path::PathBuf {
    let home = env::var("HOME").unwrap_or_else(|_| ".".into());
    std::path::PathBuf::from(home).join(".lsmtop").join("logs")
}

fn graph_opts_from(o: &OptsState) -> GraphOpts {
    let fill = match o.value("fill").as_str() {
        "bar" => Some(Fill::Bar),
        "area" => Some(Fill::Area),
        _ => None,
    };
    GraphOpts {
        squish: o.value("timebase") == "squish",
        chars: match o.value("chars").as_str() {
            "block" => Chars::Block(fill),
            "line" => Chars::Line,
            _ => Chars::Braille(fill),
        },
        colors: Mode::from_name(&o.value("colors")),
        ramp: Ramp::from_name(&o.value("ramp")),
    }
}

// ───────────────────────── headless fixture mode ─────────────────────────
// REAL fixture JSON → Sampler → dashboard text on stdout. M9's paint-parity
// hook; no network, no raw mode, exit 0.

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
    let (mem_used, mem_total) = lsmtop::gpu::sysinfo_mem();
    sampler.ram_percent = Some(if mem_total > 0.0 { mem_used / mem_total * 100.0 } else { 0.0 });

    let cfg = Config::load(None::<String>).map_err(|e| io::Error::other(e.to_string()))?;
    let opts_state = OptsState::from_config(&cfg);
    let gopts = graph_opts_from(&opts_state);

    let (rows_dash, _loaded_n, _total_n) = build_table_rows(Some(&models), &sampler.rows, true);
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
    let strip_toks = metrics_strip(true);
    let stext = strip_text(&strip_toks);
    let backend = ratatui::backend::TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend)?;
    terminal.draw(|f| {
        let s = &mut sampler;
        let run_g: f64 = s.rows.iter().map(|r| r.gen_ts.max(0.0)).sum();
        let run_p: f64 = s.rows.iter().map(|r| r.prompt_ts.max(0.0)).sum();
        let kv_now = s.kv.values().last().copied().unwrap_or(0.0);
        let apu_now = s.apu.values().last().copied().unwrap_or(0.0);
        let w = DashboardWidget {
            stats: "fixture \u{2014} offline capture",
            hotkeys: &stext,
            hotkey_toks: &strip_toks,
            opts: gopts.clone(),
            tick: s.polls,
            gen: &s.gen,
            gen_leaky: &mut s.gen_leaky,
            gen_legend: legend(&s.gen, "GEN t/s", run_g, "run"),
            prompt: &s.prompt,
            prompt_leaky: &mut s.prompt_leaky,
            prompt_legend: legend(&s.prompt, "PROMPT t/s", run_p, "run"),
            kv: &s.kv,
            kv_legend: legend(&s.kv, "KV\u{2248} %", kv_now, "now"),
            apu: &s.apu,
            apu_legend: legend(&s.apu, apu_title(), apu_now, "now"),
            vis,
            sizes,
            graph_scroll: 0,
            journal: None,
            journal_h: 0,
            rows: rows_dash.clone(),
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

struct Rt {
    config: Config,
    config_path: std::path::PathBuf,
    stack: Vec<Screen>,
    target_ix: usize,
    sampler: Sampler,
    last_models: Option<serde_json::Value>,
    last_sample_err: Option<String>,
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
    logs: Option<LogState>,
    log_child: Option<Child>,
    log_flags: Vec<Arc<AtomicBool>>,
    owned: Option<OwnedProcess>,
    collector: Option<(JoinHandle<()>, Arc<AtomicBool>)>,
    mgr_sel: usize,
    lanes_all: bool,
    unit_infos: HashMap<usize, Result<UnitInfo, String>>,
    units_polled: Instant,
    hf: Option<HfState>,
    hf_flags: Vec<Arc<AtomicBool>>,
    setedit: Option<SetEdit>,
    /// interactive rects recorded by the LAST render (mouse hit map)
    hit: Vec<(Rect, Hit)>,
    dirty: bool,
}

impl Rt {
    fn target(&self) -> serde_json::Value {
        self.config.targets().into_iter().nth(self.target_ix).unwrap_or_else(|| {
            serde_json::json!({"kind": "router", "name": "none", "base_url": "http://127.0.0.1:8085"})
        })
    }
    fn top(&self) -> Screen {
        *self.stack.last().unwrap_or(&Screen::Metrics)
    }
    /// R5: action-feedback toast eliminated by request — results live in
    /// the state the action changed (rows, chips, servers). Call sites
    /// keep their intent readable; nothing renders this.
    fn say(&mut self, _msg: impl Into<String>) {}
}

/// collector::spawn's flag: `true` = STOP (its loop is `while !stop`).
fn spawn_collector(
    target: serde_json::Value,
    tx: Sender<AppEvent>,
) -> (JoinHandle<()>, Arc<AtomicBool>) {
    let stop = Arc::new(AtomicBool::new(false));
    let handle = collector::spawn(target, tx, stop.clone());
    (handle, stop)
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
    let mut rt = Rt {
        config,
        config_path: config_path.clone(),
        stack: vec![Screen::Metrics],
        target_ix: 0,
        sampler: Sampler::new(),
        last_models: None,
        last_sample_err: None,
        opts,
        graph_scroll: 0,
        graphs_win: None,
        jpane: None,
        jpane_child: None,
        jpane_flags: Vec::new(),
        jpane_mode: "off".to_string(),
        jpane_win: None,
        logs: None,
        log_child: None,
        log_flags: Vec::new(),
        owned: None,
        collector: None,
        mgr_sel: 0,
        lanes_all: false,
        unit_infos: HashMap::new(),
        units_polled: Instant::now(),
        dirty: true,
        hf: None,
        hf_flags: Vec::new(),
        setedit: None,
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

    let (h, s) = spawn_collector(rt.target(), tx.clone());
    rt.collector = Some((h, s));

    loop {
        if rt.dirty {
            terminal.draw(|f| render(&mut rt, f))?;
            rt.dirty = false;
        }
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(AppEvent::Key(key)) => {
                if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c')
                {
                    break;
                }
                if !graph_hotkey(&mut rt, key.code) {
                    let action = App::handle_key(&rt.top(), key.code);
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
                        if rt.logs.is_none() =>
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
            Ok(AppEvent::Sample(_, Ok(data))) => {
                if let SampleData::Router(rs) = &data {
                    rt.last_models = Some(rs.models.clone());
                }
                // v9.15: 'off stops all GPU polling' — poll ONLY while on;
                // gpu_busy is set before ingest so the apu ring stays
                // tick-aligned with gen/prompt/kv.
                if rt.opts.value("gpu_graph") == "on" {
                    rt.sampler.gpu_busy = lsmtop::gpu::gpu_busy_percent().map(|v| v as f64);
                } else {
                    rt.sampler.gpu_busy = None;
                }
                if rt.units_polled.elapsed() >= Duration::from_secs(1) {
                    poll_units(&mut rt); // 1 Hz collector cadence
                }
                rt.sampler.ingest(now_secs(), &data);
                rt.last_sample_err = None;
                rt.dirty = true;
            }
            Ok(AppEvent::Sample(_, Err(e))) => {
                rt.sampler.ingest_error(&e.0);
                rt.last_sample_err = Some(e.0);
                rt.dirty = true;
            }
            Ok(AppEvent::ScrapeError(m)) => {
                rt.sampler.ingest_error(&m);
                rt.last_sample_err = Some(m);
                rt.dirty = true;
            }
            Ok(AppEvent::LogLine(l)) => {
                if let Some(ls) = rt.logs.as_mut() {
                    ls.push_line(&l);
                    rt.dirty = true;
                }
            }
            Ok(AppEvent::JPaneLine(l)) => {
                if let Some(ls) = rt.jpane.as_mut() {
                    ls.push_line(&l);
                    rt.dirty = true;
                }
            }
            Ok(AppEvent::HfTree(repo, Ok(files))) => {
                if let Some(st) = rt.hf.as_mut() {
                    st.set_listing(&repo, &files);
                    rt.dirty = true;
                }
            }
            Ok(AppEvent::HfTree(_, Err(m)) | AppEvent::HfLogLine(m)) => {
                if let Some(st) = rt.hf.as_mut() {
                    st.push_log(m);
                    rt.dirty = true;
                }
            }
            Ok(AppEvent::Tick) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if rt.top() == Screen::Manager
                    && rt.units_polled.elapsed() > Duration::from_secs_f64(2.5)
                {
                    poll_units(&mut rt);
                    rt.dirty = true;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    // graceful quit: collectors and journal followers die; TUI-owned servers
    // survive (own process group, like the app).
    if let Some((h, s)) = rt.collector.take() {
        s.store(true, Ordering::Relaxed);
        let _ = h.join();
    }
    close_logs(&mut rt);
    key_stop.store(true, Ordering::Relaxed);
    if let Some(o) = rt.owned.as_mut() {
        o.disown();
    }
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
    let (handle, stop) = spawn_collector(target.clone(), tx.clone());
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
                                "g": sl.get("n_generated_tokens"),
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

/// Settings-screen line editor (SettingsScreen Input widgets:
/// llama-server binary / models dir / hf token — the token is NEVER shown,
/// a blank commit means "keep current", exactly like the field).
#[derive(Clone, Default)]
struct SetEdit {
    fields: [String; 3],
    /// 0..2 = the three fields, 3 = the Metrics-options row (maintainer: the
    /// up/down ring must reach it so enter can open it)
    focus: usize,
    /// true only between enter (open field) and enter/esc (leave field) —
    /// outside it, letters belong to HOTKEYS, never to the fields
    editing: bool,
    msg: String,
}

/// what a mouse press lands on; rects are recorded during draw
#[derive(Clone, PartialEq)]
enum Hit {
    SettingsField(usize),
    SettingsClear,
    SettingsMetrics,
    OptsChip(usize, usize),
    MgrPanel(usize),
    /// a footer/hotkey-strip token: pressing it IS pressing the key
    Key(KeyCode),
}

/// register one clickable rect per token wherever its text is painted
/// (maintainer: "the screen hotkeys at the bottom should be clickable")
fn add_token_hits(rt: &mut Rt, x0: u16, y: u16, line: &str, tokens: &[(&str, KeyCode)]) {
    for (tok, code) in tokens {
        if let Some(off) = line.find(tok) {
            // str::find gives a BYTE offset; the screen needs a COLUMN offset
            // (footers are full of multi-byte arrows and dots)
            let col = line[..off].chars().count() as u16;
            rt.hit.push((
                Rect { x: x0 + col, y, width: tok.chars().count() as u16, height: 1 },
                Hit::Key(*code),
            ));
        }
    }
}

fn hit_at(hits: &[(Rect, Hit)], x: u16, y: u16) -> Option<usize> {
    hits.iter().rposition(|(r, _)| x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height)
}

/// Settings routing OUTSIDE edit mode is hotkey-first (maintainer): fields
/// only swallow text once enter opened them; up/down walk the ring
/// fields(0..2) + metrics-row(3); enter activates whatever is focused.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Route {
    Insert(char),
    Backspace,
    Move(isize),
    ToggleEdit,
    OpenOpts,
    ClearTok,
    /// R7: q is Back when it is not needed as a keystroke (field closed)
    EscBack,
    Ignore,
}

fn route_settings(sk: &SetKey, editing: bool, focus: usize) -> Route {
    match sk {
        SetKey::NextField => Route::Move(1),
        SetKey::PrevField => Route::Move(-1),
        SetKey::ToggleEdit => {
            if focus >= 3 {
                Route::OpenOpts
            } else {
                Route::ToggleEdit
            }
        }
        SetKey::MaybeOpts => {
            if editing {
                Route::Insert('m')
            } else {
                Route::OpenOpts
            }
        }
        SetKey::ClearTok => {
            if editing {
                Route::Insert('!')
            } else {
                Route::ClearTok
            }
        }
        SetKey::Char(c) => {
            if editing {
                Route::Insert(*c)
            } else if *c == 'q' {
                Route::EscBack
            } else {
                Route::Ignore
            }
        }
        SetKey::Backspace => {
            if editing {
                Route::Backspace
            } else {
                Route::Ignore
            }
        }
        SetKey::Commit => Route::Ignore,
    }
}

fn has_server(rt: &Rt) -> bool {
    // the side-lane launcher (owned server start/stop, Q) only exists for
    // kind:"server" targets; router-only configs must not advertise it
    rt.config.targets().iter().any(|t| {
        t.get("kind")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("server")
            == "server"
    })
}

fn open_settings(rt: &mut Rt) {
    let get = |k: &str, d: &str| {
        rt.config
            .raw()
            .get(k)
            .and_then(serde_json::Value::as_str)
            .unwrap_or(d)
            .to_string()
    };
    let home = env::var("HOME").unwrap_or_default();
    rt.setedit = Some(SetEdit {
        editing: false,
        fields: [
            get("llama_server_path", "llama-server"),
            get("models_dir", &format!("{home}/models")),
            String::new(), // the password field starts blank on purpose
        ],
        focus: 0,
        msg: String::new(),
    });
    rt.stack.push(Screen::Settings);
}

fn set_commit(rt: &mut Rt, ix: usize) {
    let val = match rt.setedit.as_ref() {
        Some(se) => se.fields[ix].trim().to_string(),
        None => return,
    };
    if ix == 2 && val.is_empty() {
        return; // placeholder: "blank = keep current"
    }
    const KEYS: [&str; 3] = ["llama_server_path", "models_dir", "hf_token"];
    rt.config.set_str(KEYS[ix], &val);
    let msg = match rt.config.save(&rt.config_path.clone()) {
        Ok(()) => "saved \u{2713}".to_string(),
        Err(e) => format!("save failed: {e}"),
    };
    if let Some(se) = rt.setedit.as_mut() {
        se.msg = msg;
    }
}

fn open_opts(rt: &mut Rt) {
    rt.opts = OptsState::from_config(&rt.config);
    rt.stack.push(Screen::MetricsOpts);
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

/// Leaving Settings for good: fields commit on blur (blank token keeps
/// current), the journal follower closes if we were stacked over it.
fn settings_leave(rt: &mut Rt) {
    if rt.setedit.is_some() {
        for f in 0..3 {
            set_commit(rt, f);
        }
    }
    if rt.top() == Screen::Logs {
        close_logs(rt);
    }
    if rt.stack.len() > 1 {
        rt.stack.pop();
    }
}

fn clear_tok(rt: &mut Rt) {
    if let Some(se) = rt.setedit.as_mut() {
        se.fields[2].clear();
        rt.config.set_str("hf_token", "");
        se.msg = match rt.config.save(&rt.config_path.clone()) {
            Ok(()) => "token cleared \u{2713}".to_string(),
            Err(e) => format!("save failed: {e}"),
        };
    }
}

/// a mouse press decoded into intent, applied
fn mouse_hit(rt: &mut Rt, hit: Hit, tx: &Sender<AppEvent>) {
    match hit {
        // footer tokens are dispatched by the caller as keypresses
        Hit::Key(_) => {}
        Hit::SettingsField(ix) => {
            let (editing, focus) = rt
                .setedit
                .as_ref()
                .map(|s| (s.editing, s.focus))
                .unwrap_or((false, 0));
            if editing {
                set_commit(rt, focus.min(2));
            }
            if let Some(se) = rt.setedit.as_mut() {
                se.focus = ix.min(3);
                se.editing = ix < 3;
            }
        }
        Hit::SettingsClear => clear_tok(rt),
        Hit::SettingsMetrics => {
            let (editing, focus) = rt
                .setedit
                .as_ref()
                .map(|s| (s.editing, s.focus))
                .unwrap_or((false, 0));
            if editing {
                set_commit(rt, focus.min(2));
            }
            if rt.top() != Screen::MetricsOpts {
                open_opts(rt);
            }
        }
        Hit::OptsChip(row, idx) => {
            if let Some(r) = rt.opts.rows.get_mut(row) {
                r.press(idx);
                rt.opts.sync_disabled();
            }
            rt.opts.sel = row;
            opts_live_save(rt, tx); // R3: a clicked chip persists at once
        }
        Hit::MgrPanel(i) => rt.mgr_sel = i,
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

fn begin_download(rt: &mut Rt, repo: &str, files: &[String], tx: &Sender<AppEvent>) {
    let Some(st) = rt.hf.as_mut() else { return };
    let Some(cli) = hf::hf_cli_path() else {
        st.push_log("\u{2717} hf CLI still not found \u{2014} run the install command above.");
        return;
    };
    let models_dir = rt
        .config
        .raw()
        .get("models_dir")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("models")
        .to_string();
    let dest = hf::hf_target_dir(&models_dir, repo);
    let _ = std::fs::create_dir_all(&dest);
    let _ = std::fs::write(dest.join(".lsmtop-hf-repo"), repo);
    let dl_dir = logs_dir().join("downloads");
    let _ = std::fs::create_dir_all(&dl_dir);
    let log_path = hf::download_log_path(&dl_dir, repo);
    let cmd = hf::build_download_cmd(&cli, repo, files, &dest);
    let token = hf::hf_token(&rt.config);
    match hf::spawn_download(&cmd, &dest, &log_path, if token.is_empty() { None } else { Some(&token) }) {
        Ok(_child) => {
            // detached by design (own process group): survives lsmtop exit
            st.push_log(format!("\u{2713} hf download started \u{2192} {}", dest.display()));
            st.push_log(format!(
                "  {repo} \u{b7} {}",
                if files.len() < 4 { files.join(", ") } else { format!("{} files", files.len()) }
            ));
            st.push_log(format!(
                "  token: {} \u{b7} log: {}",
                if token.is_empty() { "none (public)" } else { "used" },
                log_path.display()
            ));
            st.push_log("  closing lsmtop does NOT interrupt the download.");
            let stop = Arc::new(AtomicBool::new(false));
            let h = follow_file(log_path, tx.clone(), stop.clone(), AppEvent::HfLogLine);
            rt.hf_flags.push(stop);
            let _ = h;
        }
        Err(e) => st.push_log(format!("failed to spawn hf: {e}")),
    }
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
    let top = rt.top();
    match action {
        Action::Quit | Action::QuitAndStop => {
            // R7: reachable only from the metrics root (and Q on the
            // manager, where the side lane lives); q on every other page
            // is GoBack. q leaves TUI-owned servers running (they outlive
            // us in their own process group), Q stops them first.
            if rt.top() == Screen::Logs {
                close_logs(rt);
            }
            if matches!(action, Action::QuitAndStop) {
                if let Some(o) = rt.owned.as_mut() {
                    o.stop();
                }
            }
            false
        }
        Action::GoBack => {
            // R7: esc/q back out of every nested screen; only the metrics
            // root's q/Q ever quit (handled in the Quit arms).
            // R3: opts chips live-save, so leaving the opts screen is never
            // a CANCEL — its state already matches the config.
            if rt.top() == Screen::Logs {
                close_logs(rt);
            }
            if rt.stack.len() > 1 {
                rt.stack.pop();
            }
            true
        }
        Action::SwitchScreen(s) => {
            if s == Screen::Manager {
                poll_units(rt);
            }
            if s == Screen::Settings {
                open_settings(rt);
                return true;
            }
            rt.stack.push(s);
            true
        }
        Action::Journal => {
            // from the manager, the JOURNAL follows the panel under the
            // cursor
            if top == Screen::Manager && rt.mgr_sel != rt.target_ix {
                rt.target_ix = rt.mgr_sel;
                if let Some((h, s)) = rt.collector.take() {
                    s.store(true, Ordering::Relaxed);
                    let _ = h.join();
                }
                let (h, s) = spawn_collector(rt.target(), tx.clone());
                rt.collector = Some((h, s));
                rt.sampler = Sampler::new();
                rt.last_models = None;
            }
            open_logs(rt, tx.clone());
            rt.stack.push(Screen::Logs);
            true
        }
        Action::ToggleLanes => {
            rt.lanes_all = !rt.lanes_all;
            rt.say(if rt.lanes_all { "showing all lanes" } else { "showing active lanes" });
            true
        }
        Action::JournalCycle => {
            // 'j' walks the journal — the persisted tier IS the state,
            // so every press visibly advances: off -> single -> third ->
            // FULL-SCREEN journal (the pane rides single behind it) and
            // the next press repeats from third. "off" returns via the
            // Settings chip. Lowercase j only.
            let cur = rt.config.metrics().journal;
            let (next, open_full) = match cur.as_str() {
                "single" => ("third", false),
                "third" => ("single", true),
                _ => ("single", false),
            };
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
            if open_full {
                open_logs(rt, tx.clone());
                rt.stack.push(Screen::Logs);
            }
            true
        }
        Action::Download => {
            if rt.hf.is_none() {
                let cli = hf::hf_cli_path();
                let tok = hf::hf_token(&rt.config);
                let env_line = format!(
                    " hf: {}  \u{b7}  token: {}",
                    cli.clone().unwrap_or_else(|| "NOT FOUND".into()),
                    if tok.is_empty() { "none (public)" } else { "used" }
                );
                let mut st = HfState::new(env_line);
                match cli {
                    Some(_) => st
                        .push_log("paste an HF link above \u{b7} Enter lists the repo's GGUFs first"),
                    None => st.push_log(hf::install_bark()),
                }
                rt.hf = Some(st);
            }
            rt.stack.push(Screen::Hf);
            true
        }
        Action::HfKey(k) => {
            let out = rt.hf.as_mut().map(|st| st.apply(k)).unwrap_or(HfOut::Nothing);
            match out {
                HfOut::ListRepo(repo) => {
                    let tx2 = tx.clone();
                    let url = hf::repo_tree_url(&repo);
                    let tok = hf::hf_token(&rt.config);
                    thread::spawn(move || {
                        let client = reqwest::blocking::Client::builder()
                            .timeout(Duration::from_secs(15))
                            .user_agent("lsmtop")
                            .build();
                        let out: Result<Vec<hf::TreeFile>, String> = (|| {
                            let client = client.map_err(|e| e.to_string())?;
                            let mut req = client.get(&url);
                            if !tok.is_empty() {
                                req = req.bearer_auth(&tok);
                            }
                            let resp = req.send().map_err(|e| format!("API error: {e}"))?;
                            let code = resp.status().as_u16();
                            if let Some(hint) = hf::tree_status_hint(code) {
                                return Err(hint.to_string());
                            }
                            if !resp.status().is_success() {
                                return Err(format!("API error: HTTP {code}"));
                            }
                            let v: serde_json::Value =
                                resp.json().map_err(|e| format!("API error: {e}"))?;
                            Ok(hf::parse_gguf_tree(&v))
                        })();
                        let _ = tx2.send(AppEvent::HfTree(repo, out));
                    });
                }
                HfOut::Download(repo, files) => begin_download(rt, &repo, &files, tx),
                _ => {}
            }
            true
        }
        Action::Refresh => {
            poll_units(rt);
            true
        }
        Action::MgrNext => {
            if top == Screen::Manager {
                let n = rt.config.targets().len().max(1);
                rt.mgr_sel = (rt.mgr_sel + 1) % n;
            } else {
                rt.opts.cycle(1);
                opts_live_save(rt, tx); // R3
            }
            true
        }
        Action::MgrPrev => {
            if top == Screen::Manager {
                let n = rt.config.targets().len().max(1);
                rt.mgr_sel = (rt.mgr_sel + n - 1) % n;
            } else {
                rt.opts.cycle(-1);
                opts_live_save(rt, tx); // R3
            }
            true
        }
        Action::OpenOpts => {
            open_opts(rt);
            true
        }
        Action::OptsUp => {
            rt.opts.up();
            true
        }
        Action::OptsDown => {
            rt.opts.down();
            true
        }
        Action::OptsCycle => {
            rt.opts.cycle(1);
            opts_live_save(rt, tx); // R3: a chip change persists at once
            true
        }
        Action::SetEsc => {
            let editing = rt.setedit.as_ref().map(|s| s.editing).unwrap_or(false);
            if editing {
                let f = rt.setedit.as_ref().map(|s| s.focus).unwrap_or(0).min(2);
                set_commit(rt, f);
                if let Some(se) = rt.setedit.as_mut() {
                    se.editing = false;
                }
            } else {
                settings_leave(rt);
            }
            true
        }
        Action::SelectTarget => {
            if rt.top() == Screen::Manager {
                rt.target_ix = rt.mgr_sel;
                if let Some((h, s)) = rt.collector.take() {
                    s.store(true, Ordering::Relaxed);
                    let _ = h.join();
                }
                let (h, s) = spawn_collector(rt.target(), tx.clone());
                rt.collector = Some((h, s));
                rt.sampler = Sampler::new();
                rt.last_models = None;
                rt.stack = vec![Screen::Metrics];
            }
            true
        }
        Action::Start | Action::Stop => {
            let verb = if matches!(action, Action::Start) { "start" } else { "stop" };
            let t = rt.config.targets().get(rt.mgr_sel).cloned().unwrap_or_default();
            let running = rt.owned.as_mut().map(|o| o.is_running()).unwrap_or(false);
            match plan_action(&t, verb, running) {
                ActionPlan::ExecUnit(argv) => match RealRunner.run(&argv, Duration::from_secs(5)) {
                    Ok(_) => rt.say(format!("{verb} ok (user scope)")),
                    Err(e) => rt.say(format!("{verb} failed: {e}")),
                },
                ActionPlan::Handoff(cmd) => rt.say(format!("run yourself: {cmd}")),
                ActionPlan::OwnedStart => {
                    let ix = rt.mgr_sel;
                    let name = t
                        .get("name")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("server")
                        .to_string();
                    let host = t
                        .get("host")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("127.0.0.1")
                        .to_string();
                    let port = t
                        .get("port")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(8080) as u16;
                    let flags = t.get("flags").cloned().unwrap_or_else(|| serde_json::json!({}));
                    let bin = flags
                        .get("server_bin")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("llama-server")
                        .to_string();
                    let model = flags
                        .get("model")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let mut op = OwnedProcess::new(ix as u32, &name, port, host, flags, logs_dir());
                    match op.start(&bin, &model) {
                        Ok(()) => {
                            rt.say(format!("started {name} (port {port})"));
                            rt.owned = Some(op);
                        }
                        Err(e) => rt.say(format!("start failed: {e}")),
                    }
                }
                ActionPlan::OwnedStop => {
                    if let Some(o) = rt.owned.as_mut() {
                        o.stop();
                        rt.say("stopped");
                    }
                }
                ActionPlan::Bark(m) => rt.say(m),
            }
            poll_units(rt);
            true
        }
        Action::Scroll(d) => {
            if let Some(ls) = rt.logs.as_mut() {
                ls.scroll_by(if d == isize::MIN { -1_000_000 } else { d });
            }
            true
        }
        Action::Clear => {
            if let Some(ls) = rt.logs.as_mut() {
                ls.clear_view();
            }
            true
        }
        Action::SetEdit(sk) => {
            let (editing, focus) = rt
                .setedit
                .as_ref()
                .map(|s| (s.editing, s.focus))
                .unwrap_or((false, 0));
            match route_settings(&sk, editing, focus) {
                Route::Move(d) => {
                    if editing {
                        set_commit(rt, focus.min(2));
                    }
                    if let Some(se) = rt.setedit.as_mut() {
                        // ring = 3 fields + the metrics-options row (maintainer)
                        se.focus = ((focus as isize + d).rem_euclid(4)) as usize;
                        se.editing = false;
                    }
                }
                Route::Insert(c) => {
                    if let Some(se) = rt.setedit.as_mut() {
                        se.fields[focus.min(2)].push(c);
                    }
                }
                Route::Backspace => {
                    if let Some(se) = rt.setedit.as_mut() {
                        se.fields[focus.min(2)].pop();
                    }
                }
                Route::ToggleEdit => {
                    if editing {
                        set_commit(rt, focus.min(2));
                    }
                    if let Some(se) = rt.setedit.as_mut() {
                        se.editing = !editing;
                    }
                }
                Route::OpenOpts => {
                    if editing {
                        set_commit(rt, focus.min(2));
                    }
                    open_opts(rt);
                }
                Route::ClearTok => clear_tok(rt),
                Route::EscBack => {
                    settings_leave(rt);
                    return true;
                }
                Route::Ignore => {}
            }
            true
        }
        Action::Hup => {
            // _router_hup parity: SIGHUP the MainPID directly;
            // permission problems become a printed hand-off, never sudo.
            let t = rt.config.targets().get(rt.mgr_sel).cloned().unwrap_or_default();
            let unit = t
                .get("unit")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string();
            if unit.is_empty() {
                rt.say("owned target: s start / x stop \u{2014} nothing to HUP");
            } else {
                let pid = rt
                    .unit_infos
                    .get(&rt.mgr_sel)
                    .and_then(|r| r.as_ref().ok())
                    .and_then(|u| u.main_pid);
                match pid {
                    None => rt.say(format!("{unit}: no MainPID known \u{2014} refresh (r) and retry")),
                    Some(p) => {
                        let ok = Command::new("kill")
                            .args(["-HUP", &p.to_string()])
                            .status()
                            .map(|s| s.success())
                            .unwrap_or(false);
                        rt.say(if ok {
                            format!(
                                "SIGHUP sent to PID {p} \u{2014} router restarting, loaded lanes evicted"
                            )
                        } else {
                            format!("permission denied \u{2014} run yourself: sudo kill -HUP {p}")
                        });
                    }
                }
            }
            true
        }
        Action::ShowCmds => {
            let t = rt.config.targets().get(rt.mgr_sel).cloned().unwrap_or_default();
            let unit = t
                .get("unit")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string();
            let scope = t
                .get("scope")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("system");
            if unit.is_empty() {
                rt.say("owned target: no systemctl unit");
            } else {
                let u = if scope == "user" { "--user " } else { "" };
                rt.say(format!(
                    "run yourself: systemctl {u}status {unit} \u{b7} systemctl {u}restart {unit}"
                ));
            }
            true
        }
        Action::Cancel | Action::Noop => true,
    }
}

/// R2: every screen's bottom strip starts with its back action — the same
/// spot for the mouse on Settings and the journal.
pub const SETTING_FOOTER: &str =
    " esc Back saves \u{b7} \u{2191}\u{2193} Field \u{b7} enter Edit/Open \u{b7} m Metrics options \u{b7} ! clear token ";

/// G2: graph letters act DIRECTLY on the metrics screen — no focus mode.
/// Lowercase cycles that pane's size (full \u{2192} half \u{2192} single), Shift
/// toggles its visibility; g = GEN (Llama moved to 'L'), u = GPU. Every
/// change persists through config, same validated metric gate as Settings.
fn graph_hotkey(rt: &mut Rt, code: KeyCode) -> bool {
    if rt.top() != Screen::Metrics {
        return false;
    }
    let (cfg_key, name, what): (&str, &str, &str) = match code {
        KeyCode::Char('g') => ("gen_size", "GEN", "size"),
        KeyCode::Char('p') => ("prompt_size", "PROMPT", "size"),
        KeyCode::Char('v') => ("kv_size", "KV", "size"),
        KeyCode::Char('u') => ("gpu_size", "GPU", "size"),
        KeyCode::Char('G') => ("gen_graph", "GEN", "graph"),
        KeyCode::Char('P') => ("prompt_graph", "PROMPT", "graph"),
        KeyCode::Char('V') => ("kv_graph", "KV", "graph"),
        KeyCode::Char('U') => ("gpu_graph", "GPU", "graph"),
        _ => return false,
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

fn table_cells(r: &lsmtop::sampling::LaneRow) -> [String; 9] {
    // exact formats; falsy rates render the em dash.
    // B1: TOTAL tk rides between PROMPT and GEN.
    [
        r.model.clone(),
        r.act.clone(),
        if r.prompt_ts > 0.0 { format!("{:.1}", r.prompt_ts) } else { "\u{2014}".into() },
        fmt_total_tk(r.total_tk),
        if r.gen_ts > 0.0 { format!("{:.1}", r.gen_ts) } else { "\u{2014}".into() },
        format!("{:.1}", r.kv),
        r.cache_pct.map(|c| format!("{:.0}%", c)).unwrap_or_else(|| "\u{2014}".into()),
        r.ctx_k.map(|k| format!("{k}k")).unwrap_or_else(|| "\u{2014}".into()),
        r.acc.map(|a| format!("{:.0}%", a)).unwrap_or_else(|| "\u{2014}".into()),
    ]
}

/// (rows, loaded_n, total_n) — table iterates /models status;
/// loaded lanes ride the sampler rows, unloaded show dashes with the
/// status string parked in ACCEPT; `show_all` lists everything.
fn build_table_rows(
    models: Option<&serde_json::Value>,
    rows: &[lsmtop::sampling::LaneRow],
    show_all: bool,
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
        for m in arr {
            let mid = m.get("id").and_then(|v| v.as_str()).unwrap_or("?").to_string();
            let sval = m
                .get("status")
                .and_then(|s| s.get("value"))
                .and_then(|v| v.as_str())
                .unwrap_or("?")
                .to_string();
            total_n += 1;
            let loaded = sval == "loaded";
            if loaded {
                loaded_n += 1;
            }
            if !loaded && !show_all {
                continue;
            }
            match rows.iter().find(|r| r.model == mid) {
                Some(r) => out.push(DashRow { cells: table_cells(r) }),
                None => out.push(DashRow {
                    cells: [
                        mid,
                        "\u{2014}".into(),
                        "\u{2014}".into(),
                        "\u{2014}".into(), // TOTAL: unloaded has no live counter
                        "\u{2014}".into(),
                        "\u{2014}".into(),
                        "\u{2014}".into(),
                        "\u{2014}".into(),
                        sval,
                    ],
                }),
            }
        }
    } else {
        for r in rows {
            total_n += 1;
            loaded_n += 1;
            out.push(DashRow { cells: table_cells(r) });
        }
    }
    (out, loaded_n, total_n)
}

fn legend<'a>(
    ring: &lsmtop::hist::Ring,
    title: &'a str,
    head_val: f64,
    head_word: &'a str,
) -> Legend<'a> {
    let (avg, max) = ring_stats(ring);
    Legend { title, head: (head_word, head_val), avg, max }
}

/// Header parity: full-width accent bar, app
/// title centered, live clock right. Entirely missing until the pty-vs-pty
/// audit (maintainer: "the entire thing feels barebones").
fn draw_header(buf: &mut ratatui::buffer::Buffer, area: Rect, banner: Option<&str>) {
    let bar = Style::default()
        .bg(Color::Rgb(9, 84, 170))
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);
    for x in area.x..area.x + area.width {
        buf[(x, area.y)].set_char(' ').set_style(bar);
    }
    if area.width >= 12 {
        buf.set_string(area.x, area.y, " \u{2b58}", bar);
        // The banner (router · lanes · unit · PID) rides WHERE the app
        // title used to sit — the title line was superfluous, and this
        // saves a row on the metrics screen.
        let title = banner.unwrap_or(" lsmtop \u{2014} llama.cpp metrics & mgmt ");
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

fn render(rt: &mut Rt, f: &mut ratatui::Frame) {
    rt.hit.clear(); // rebuilt by the draws below (last-frame hit map)
    let full = f.area();
    // On the metrics screen the banner line moves UP into the header bar
    // (title saved, one row saved); build it before the header draws.
    let head_banner: Option<String> = if matches!(rt.top(), Screen::Metrics) && full.height >= 5 {
        let t = rt.target();
        let name = t
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("router")
            .to_string();
        let (_, loaded_n, total_n) =
            build_table_rows(rt.last_models.as_ref(), &rt.sampler.rows, rt.lanes_all);
        let uinfo = rt.unit_infos.get(&rt.target_ix).and_then(|r| r.as_ref().ok());
        Some(format!(
            " {name} \u{2014} {loaded_n}/{total_n} lanes loaded \u{00b7} unit {} \u{00b7} PID {} ",
            uinfo.map(|u| u.active_state.as_str()).unwrap_or("?"),
            uinfo.and_then(|u| u.main_pid).unwrap_or(0)
        ))
    } else {
        None
    };
    let body = if full.height >= 5 {
        draw_header(
            f.buffer_mut(),
            Rect { x: full.x, y: full.y, width: full.width, height: 1 },
            head_banner.as_deref(),
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
            let (rows_dash, _loaded_n, _total_n) =
                build_table_rows(rt.last_models.as_ref(), &rt.sampler.rows, rt.lanes_all);
            let rows_len_win = rows_dash.len();
            let (used, total, avail) = lsmtop::gpu::mem_gib();
            let stats_base = if vis[3] {
                let gtxt = rt
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
                    "RAM {used:.1}/{total:.0} GiB used, {avail:.0} avail  \u{00b7} GPU graph off (Settings \u{25b8} Metrics)"
                )
            };
            let dev = lsmtop::gpu::device_labels().into_iter().next().unwrap_or_default();
            let stats = if dev.is_empty() {
                stats_base
            } else {
                format!("{stats_base} \u{00b7} {dev}")
            };
            let hs = has_server(rt); // before the sampler borrow
            // G2: the strip is the primary UI — accented letters, each a
            // mouse target, no focus mode anywhere.
            let strip_toks: Vec<StripTok> = metrics_strip(hs);
            let (stext, strip_cols) = strip_layout(&strip_toks);
            // B4/G1: pane size law against the live body; the journal
            // flexes before graphs lose their per-pane minimums. The
            // single tier rides the newest line WHOLE: its wrapped row
            // count sizes the pane (1, or 2 for a long line).
            let jmode = rt.config.metrics().journal;
            let pane_w = body.width.saturating_sub(2) as usize;
            let single_rows = if jmode == "single" {
                rt.jpane
                    .as_ref()
                    .and_then(|ls| ls.lines.back())
                    .map(|l| journal_single_rows(Some(l), pane_w))
                    .unwrap_or(1)
            } else {
                1
            };
            let jh = journal_pane_h(body.height, &jmode, graphs_min(&vis, &sizes), single_rows);
            let journal = if jh > 0 {
                rt.jpane.as_ref().map(|ls| JournalPane {
                    lines: &ls.lines,
                    scroll: ls.scroll,
                })
            } else {
                None
            };
            {
                let s = &mut rt.sampler;
                let run_g: f64 = s.rows.iter().map(|r| r.gen_ts.max(0.0)).sum();
                let run_p: f64 = s.rows.iter().map(|r| r.prompt_ts.max(0.0)).sum();
                let kv_now = s.kv.values().last().copied().unwrap_or(0.0);
                let apu_now = s.apu.values().last().copied().unwrap_or(0.0);
                let w = DashboardWidget {
                                        stats: &stats,
                    hotkeys: &stext,
                    hotkey_toks: &strip_toks,
                    opts: gopts,
                    tick: s.polls,
                    gen: &s.gen,
                    gen_leaky: &mut s.gen_leaky,
                    gen_legend: legend(&s.gen, "GEN t/s", run_g, "run"),
                    prompt: &s.prompt,
                    prompt_leaky: &mut s.prompt_leaky,
                    prompt_legend: legend(&s.prompt, "PROMPT t/s", run_p, "run"),
                    kv: &s.kv,
                    kv_legend: legend(&s.kv, "KV\u{2248} %", kv_now, "now"),
                    apu: &s.apu,
                    apu_legend: legend(&s.apu, apu_title(), apu_now, "now"),
                    vis,
                    sizes,
                    graph_scroll: rt.graph_scroll,
                    journal,
                    journal_h: jh,
                    rows: rows_dash,
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
                    graph_layout(avail, rows_len_win as u16, graphs_min(&vis, &sizes));
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
        Screen::Manager => {
            let targets = rt.config.targets();
            let infos: Vec<PanelInfo> = targets
                .iter()
                .enumerate()
                .map(|(i, t)| {
                    let kind =
                        t.get("kind").and_then(serde_json::Value::as_str).unwrap_or("server");
                    let unit_state = rt.unit_infos.get(&i).map(|r| match r {
                        Ok(u) => format!("{} ({})", u.active_state, u.sub_state),
                        Err(e) => e.clone(),
                    });
                    let router_lines = if kind == "router" && i == rt.target_ix {
                        rt.last_models.as_ref().map(router_lane_lines).unwrap_or_default()
                    } else {
                        Vec::new()
                    };
                    PanelInfo {
                        unit_state,
                        owned_running: i == rt.mgr_sel
                            && rt.owned.as_mut().map(|o| o.is_running()).unwrap_or(false),
                        router_lines,
                        sample_error: if i == rt.target_ix {
                            rt.last_sample_err.as_deref()
                        } else {
                            None
                        },
                    }
                })
                .collect();
            // panel rects for the mouse (mirror ManagerWidget's split)
            let outer = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(3), Constraint::Length(1)])
                .split(body);
            let shown = targets.len().min(3);
            let mut cs: Vec<Constraint> = (0..shown.max(1)).map(|_| Constraint::Fill(1)).collect();
            if targets.len() > 3 {
                cs.push(Constraint::Length(9));
            }
            let cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints(cs)
                .split(outer[0]);
            for (i, r) in cols.iter().enumerate().take(shown) {
                rt.hit.push((*r, Hit::MgrPanel(i)));
            }
            let has_srv = has_server(rt);
            f.render_widget(
                ManagerWidget { targets: &targets, infos, selected: rt.mgr_sel, has_server: has_srv },
                body,
            );
            {
                let foot = footer_text(has_srv);
                let mut toks: Vec<(&str, KeyCode)> = vec![
                    ("esc Back", KeyCode::Esc),
                    ("r Refresh", KeyCode::Char('r')),
                    ("u HUP", KeyCode::Char('u')),
                    ("c cmds", KeyCode::Char('c')),
                    ("h HF", KeyCode::Char('h')),
                    ("S Settings", KeyCode::Char('S')),
                ];
                if has_srv {
                    toks.push(("s Start", KeyCode::Char('s')));
                    toks.push(("x Stop", KeyCode::Char('x')));
                    toks.push(("Q Quit+Stop", KeyCode::Char('Q')));
                }
                add_token_hits(rt, outer[1].x, outer[1].y, foot, &toks);
            }
        }
        Screen::Settings => {
            // SettingsScreen parity: editable binary/models-dir/token
            // fields (token masked, blank = keep), Clear, chip-panel button,
            // and the plain-words help block.
            let se = rt.setedit.clone().unwrap_or_default();
            let tok_set = rt
                .config
                .hf_token()
                .map(|t| !t.trim().is_empty())
                .unwrap_or(false);
            let w = (body.width.min(76).saturating_sub(6)).max(16) as usize;
            let mut lines: Vec<Line> = Vec::new();
            lines.push(Line::styled(
                " Settings ".to_string(),
                Style::default().add_modifier(Modifier::BOLD),
            ));
            lines.push(Line::from(""));
            let labels: [(&str, &str); 3] = [
                ("llama-server binary (path or name in PATH)", ""),
                ("Models directory", ""),
                (
                    "HuggingFace token (optional, never shown)",
                    if tok_set { " \u{2713} set" } else { "" },
                ),
            ];
            let mut box_rows: Vec<u16> = Vec::new();
            let mut clear_row: Option<u16> = None;
            for (ix, (label, suffix)) in labels.iter().enumerate() {
                box_rows.push(lines.len() as u16 + 1); // the \u{250c} border line
                let lst = if se.focus == ix {
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                lines.push(Line::from(vec![Span::styled(
                    format!("  {label}{suffix}:"),
                    lst,
                )]));
                let shown = if ix == 2 {
                    "*".repeat(se.fields[2].chars().count())
                } else {
                    se.fields[ix].clone()
                };
                let mut mid: String = shown
                    .chars()
                    .take(w.saturating_sub(if se.focus == ix && se.editing { 1 } else { 0 }))
                    .collect();
                if se.focus == ix && se.editing {
                    mid.push('\u{258a}'); // caret: the field is OPEN
                }
                let bs = if se.focus == ix && se.editing {
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
                } else if se.focus == ix {
                    Style::default().fg(Color::Cyan)
                } else {
                    Style::default().fg(Color::DarkGray)
                };
                let pad = w.saturating_sub(mid.chars().count());
                // open field = full-width highlight bar (maintainer: clicked or
                // opened fields must visibly highlight). REVERSE survives
                // every terminal AND every pty test harness (pyte cannot
                // see SGR-100 bright backgrounds).
                let cs = if se.focus == ix && se.editing {
                    bs.add_modifier(Modifier::REVERSED)
                } else {
                    bs
                };
                lines.push(Line::styled(
                    format!("  \u{250c}{}\u{2510}", "\u{2500}".repeat(w + 2)),
                    bs,
                ));
                lines.push(Line::styled(
                    format!("  \u{2502} {mid}{}\u{2502}", " ".repeat(pad + 1)),
                    cs,
                ));
                lines.push(Line::styled(
                    format!("  \u{2514}{}\u{2518}", "\u{2500}".repeat(w + 2)),
                    bs,
                ));
                if ix == 2 {
                    clear_row = Some(lines.len() as u16);
                    let mut row = vec![Span::styled(
                        "   [ ! ] clear token",
                        Style::default().fg(Color::Yellow),
                    )];
                    if !se.msg.is_empty() {
                        row.push(Span::styled(
                            format!("   {}", se.msg),
                            Style::default().fg(Color::Green),
                        ));
                    }
                    lines.push(Line::from(row));
                }
                lines.push(Line::from(""));
            }
            let metrics_row = lines.len() as u16;
            let mstyle = if se.focus == 3 {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD | Modifier::REVERSED)
            } else {
                Style::default().add_modifier(Modifier::BOLD)
            };
            lines.push(Line::from(vec![
                Span::styled(
                    format!(
                        "  {}Metrics graph options \u{25b8}",
                        if se.focus == 3 { "\u{25b8} " } else { "" }
                    ),
                    mstyle,
                ),
                Span::styled(
                    if se.focus == 3 { "   (enter)" } else { "   (m)" },
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
            lines.push(Line::from(""));
            let help = [
                "  Dashboard, plain words",
                "  \u{b7} curves = live /slots, polled on each child's port",
                "  \u{b7} PROMPT digests \u{b7} GEN rests at the rail during prefill",
                "  \u{b7} run = journal run-average \u{b7} avg = 60 s \u{b7} max = 10 min",
                "  \u{b7} fill n/a for old-school line style",
                "  Keys: a lanes \u{b7} l journal \u{b7} g manager \u{b7} esc back",
                "  flags: \u{2699} per server panel \u{b7} targets: edit ~/.lsmtop/config.json",
            ];
            for hl in help {
                lines.push(Line::styled(hl.to_string(), Style::default().fg(Color::DarkGray)));
            }
            f.render_widget(
                Paragraph::new(lines),
                Rect { x: body.x, y: body.y, width: body.width, height: body.height.saturating_sub(1) },
            );
            // mouse hits: field boxes, clear button, metrics row
            for (ix, br) in box_rows.iter().enumerate() {
                rt.hit.push((
                    Rect { x: body.x + 2, y: body.y + br, width: (w + 5) as u16, height: 3 },
                    Hit::SettingsField(ix),
                ));
            }
            if let Some(cr) = clear_row {
                rt.hit.push((
                    Rect { x: body.x + 2, y: body.y + cr, width: 22, height: 1 },
                    Hit::SettingsClear,
                ));
            }
            rt.hit.push((
                Rect { x: body.x, y: body.y + metrics_row, width: body.width, height: 1 },
                Hit::SettingsMetrics,
            ));
            add_token_hits(
                rt,
                body.x,
                body.y + body.height.saturating_sub(1),
                SETTING_FOOTER,
                &[
                    ("esc Back", KeyCode::Esc),
                    ("m Metrics options", KeyCode::Char('m')),
                    ("! clear token", KeyCode::Char('!')),
                ],
            );
            f.render_widget(
                Paragraph::new(SETTING_FOOTER).style(Style::default().fg(Color::DarkGray)),
                Rect {
                    x: body.x,
                    y: body.y + body.height.saturating_sub(1),
                    width: body.width,
                    height: 1,
                },
            );
        }
        Screen::MetricsOpts => {
            // chip rects for the mouse — MUST mirror OptsWidget geometry:
            // row i chips sit at body.y + 1 + 2i, chips start x = body.x + 14,
            // labels "[ v ]" separated by one space
            for (i, row) in rt.opts.rows.iter().enumerate() {
                let ry = body.y + 1 + 2 * i as u16;
                let mut cx = body.x + 14;
                let lim = body.x + body.width;
                for (j, v) in row.values.iter().enumerate() {
                    let lw = v.chars().count() as u16 + 4;
                    if cx + lw > lim {
                        break;
                    }
                    rt.hit.push((Rect { x: cx, y: ry, width: lw, height: 1 }, Hit::OptsChip(i, j)));
                    cx += lw + 1;
                }
            }
            f.render_widget(OptsWidget(&rt.opts), body);
            // the widget draws its footer directly under the last chip row
            // (row i at body.y+1+2i), NOT at the bottom of the screen
            add_token_hits(
                rt,
                body.x,
                body.y + 1 + 2 * rt.opts.rows.len() as u16,
                OPTS_FOOTER,
                &[("esc Back", KeyCode::Esc)],
            );
        }
        Screen::Logs => {
            if let Some(ls) = rt.logs.as_ref() {
                // R2: the back hint lives in the bottom strip, left-most,
                // like every other screen (was chopped into the box title).
                let logs_area = Rect { height: body.height.saturating_sub(1), ..body };
                f.render_widget(LogsWidget(ls), logs_area);
                let foot = " esc Back · q Back · c Clear view · wheel Scroll ";
                let fy = body.y + body.height.saturating_sub(1);
                add_token_hits(
                    rt,
                    body.x,
                    fy,
                    foot,
                    &[
                        ("esc Back", KeyCode::Esc),
                        ("q Back", KeyCode::Char('q')),
                        ("c Clear view", KeyCode::Char('c')),
                    ],
                );
                f.render_widget(
                    Paragraph::new(foot).style(Style::default().fg(Color::DarkGray)),
                    Rect { x: body.x, y: fy, width: body.width, height: 1 },
                );
            }
        }
        Screen::Hf => {
            if let Some(st) = rt.hf.as_ref() {
                f.render_widget(HfWidget(st), body);
                add_token_hits(
                    rt,
                    body.x,
                    body.y + body.height.saturating_sub(1),
                    " esc Back · Enter List · d Download · backspace Edit ",
                    &[
                        ("esc Back", KeyCode::Esc),
                        ("Enter List", KeyCode::Enter),
                        ("d Download", KeyCode::Char('d')),
                    ],
                );
            }
        }
        Screen::Confirm => {}
    }
}

#[cfg(test)]
mod shell_tests {
    //! Settings routing is hotkey-first OUTSIDE edit mode (maintainer), and the
    //! mouse hit map must resolve to the widget the pixels show.
    use super::*;

    fn sk(k: SetKey) -> SetKey {
        k
    }

    #[test]
    fn closed_fields_never_swallow_hotkey_letters() {
        // app.rs binds 'm' to MaybeOpts; a raw Char('m') here would be
        // another letter — fields must NOT swallow it while closed
        assert_eq!(route_settings(&sk(SetKey::MaybeOpts), false, 0), Route::OpenOpts);
        assert_eq!(route_settings(&sk(SetKey::Char('m')), false, 0), Route::Ignore);
        assert_eq!(route_settings(&sk(SetKey::Char('x')), false, 0), Route::Ignore);
        assert_eq!(route_settings(&sk(SetKey::Backspace), false, 0), Route::Ignore);
        assert_eq!(route_settings(&sk(SetKey::ClearTok), false, 1), Route::ClearTok);
    }

    #[test]
    fn open_field_captures_everything_including_m_and_bang() {
        assert_eq!(route_settings(&sk(SetKey::Char('m')), true, 0), Route::Insert('m'));
        assert_eq!(route_settings(&sk(SetKey::MaybeOpts), true, 2), Route::Insert('m'));
        assert_eq!(route_settings(&sk(SetKey::ClearTok), true, 2), Route::Insert('!'));
        assert_eq!(route_settings(&sk(SetKey::Char('/')), true, 1), Route::Insert('/'));
        assert_eq!(route_settings(&sk(SetKey::Backspace), true, 1), Route::Backspace);
    }

    #[test]
    fn enter_opens_fields_and_activates_the_metrics_row() {
        assert_eq!(route_settings(&sk(SetKey::ToggleEdit), false, 0), Route::ToggleEdit);
        assert_eq!(route_settings(&sk(SetKey::ToggleEdit), true, 0), Route::ToggleEdit);
        // focus 3 = the metrics-options row: enter OPENS it (maintainer)
        assert_eq!(route_settings(&sk(SetKey::ToggleEdit), false, 3), Route::OpenOpts);
    }

    #[test]
    fn up_down_ring_includes_the_metrics_row() {
        // ring math is applied in the handler as (focus + d).rem_euclid(4)
        let mut focus = 0usize;
        for _ in 0..4 {
            focus = ((focus as isize - 1).rem_euclid(4)) as usize;
        }
        assert_eq!(focus, 0);
        assert_eq!(((2isize + 1).rem_euclid(4)) as usize, 3); // Down lands on metrics
        assert_eq!(((3isize + 1).rem_euclid(4)) as usize, 0); // and wraps back
    }

    #[test]
    fn hit_map_resolves_inside_only() {
        let r = Rect { x: 2, y: 3, width: 10, height: 1 };
        let hits = vec![(r, Hit::SettingsMetrics)];
        assert_eq!(hit_at(&hits, 2, 3), Some(0));
        assert_eq!(hit_at(&hits, 11, 3), Some(0));
        assert_eq!(hit_at(&hits, 12, 3), None);
        assert_eq!(hit_at(&hits, 2, 4), None);
    }
}
