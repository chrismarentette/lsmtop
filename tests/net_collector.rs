use lsmtop::app::AppEvent;
use lsmtop::net::collector::spawn;
use serde_json::json;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

#[test]
fn test_collector_thread() {
    let (tx, rx) = std::sync::mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_clone = Arc::clone(&stop);

    let target = json!({
        "kind": "server",
        "host": "127.0.0.1",
        "port": 1, // fast connection refused
    });

    let handle = spawn(target, tx, stop_clone, 500);
    let mut errors = 0;
    
    let t0 = Instant::now();
    while errors < 3 {
        if let Ok(AppEvent::Sample(_, Err(_))) = rx.recv() {
            errors += 1;
        }
        if t0.elapsed().as_secs() > 5 {
            break; // timeout
        }
    }
    
    assert!(errors >= 3);
    stop.store(true, Ordering::Relaxed);
    
    let t1 = Instant::now();
    handle.join().unwrap();
    assert!(t1.elapsed().as_secs_f64() < 1.5);
}

#[test]
fn collector_honours_the_tick_cadence() {
    // Settings > Tick rate drives the scrape loop. Connection-refused
    // scrapes answer instantly, so events land ON the tick: 500 ms ->
    // several events in the window; a 5000 ms tick stays at ONE. The
    // retired hardcoded 1 s cadence fails the second half.
    use std::time::Duration;
    let run = |tick: u32, window_ms: u64| -> usize {
        let (tx, rx) = std::sync::mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let sc = stop.clone();
        let target = json!({"kind": "server", "host": "127.0.0.1", "port": 1});
        let handle = spawn(target, tx, sc, tick);
        let t0 = Instant::now();
        let mut n = 0;
        while t0.elapsed() < Duration::from_millis(window_ms) {
            if let Ok(AppEvent::Sample(_, Err(_))) =
                rx.recv_timeout(Duration::from_millis(300))
            {
                n += 1;
            }
        }
        stop.store(true, Ordering::Relaxed);
        let _ = handle.join();
        n
    };
    let fast = run(500, 2600);
    assert!((4..=9).contains(&fast), "tick 500 ms -> ~5 events in 2.6 s, got {fast}");
    let slow = run(5000, 2600);
    assert_eq!(slow, 1, "tick 5000 ms -> the FIRST event only, got {slow}");
}
