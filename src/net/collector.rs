use crate::app::{AppEvent, SampleData, ScrapeError};
use crate::net::Scraper;
use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// One scrape per `tick_ms` (Settings ▸ Tick rate; caller passes the
/// validated config value, bounds re-applied defensively — a 0 here would
/// hot-loop the engine).
pub fn spawn(
    target: Value,
    tx: std::sync::mpsc::Sender<AppEvent>,
    stop: Arc<AtomicBool>,
    tick_ms: u32,
) -> JoinHandle<()> {
    let tick_ms = crate::config::normalize_tick_ms(tick_ms);
    thread::spawn(move || {
        let scraper = Scraper::new();
        let kind = target.get("kind").and_then(|v| v.as_str()).unwrap_or("server").to_string();
        
        while !stop.load(Ordering::Relaxed) {
            let t0 = Instant::now();

            let sample = if kind == "router" {
                // Parse base_url
                let base_url = target.get("base_url").and_then(|v| v.as_str()).unwrap_or("http://127.0.0.1:8085");
                if let Ok(url) = reqwest::Url::parse(base_url) {
                    let host = url.host_str().unwrap_or("127.0.0.1");
                    let port = url.port_or_known_default().unwrap_or(8085);
                    match scraper.scrape_router(host, port) {
                        Some(res) => Ok(SampleData::Router(res)),
                        None => Err(ScrapeError("Router unreachable".to_string())),
                    }
                } else {
                    Err(ScrapeError("Invalid router URL".to_string()))
                }
            } else {
                let host = target.get("host").and_then(|v| v.as_str()).unwrap_or("127.0.0.1");
                let port = target.get("port").and_then(|v| v.as_u64()).unwrap_or(9931) as u16;
                match scraper.scrape_server(host, port) {
                    Some(res) => Ok(SampleData::Server(res)),
                    None => Err(ScrapeError("Server unreachable".to_string())),
                }
            };

            let _ = tx.send(AppEvent::Sample(0, sample));

            let elapsed = t0.elapsed();
            let target_duration = Duration::from_millis(tick_ms as u64);
            let min_duration = Duration::from_millis(200);

            let sleep_time = if elapsed < target_duration {
                let rem = target_duration - elapsed;
                std::cmp::max(rem, min_duration)
            } else {
                min_duration
            };
            // Sleep in 50 ms chunks: a Settings tick change JOINS this
            // thread from the UI loop, and one full uninterruptible sleep
            // would freeze the interface for a whole tick.
            let wake = Instant::now() + sleep_time;
            while !stop.load(Ordering::Relaxed) {
                let rem = wake.saturating_duration_since(Instant::now());
                if rem.is_zero() {
                    break;
                }
                thread::sleep(std::cmp::min(rem, Duration::from_millis(50)));
            }
        }
    })
}
