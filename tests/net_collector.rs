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

    let handle = spawn(target, tx, stop_clone);
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
