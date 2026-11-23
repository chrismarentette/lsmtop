use crate::app::{AppEvent, EngineMode, SampleData, ScrapeError};
use crate::net::{classify_models, Scraper};
use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// (host, port) from either spelling: router targets carry base_url,
/// server targets carry host+port; auto targets may carry either.
pub fn target_addr(t: &Value) -> (String, u16) {
    if let Some(bu) = t.get("base_url").and_then(|v| v.as_str()) {
        if let Ok(url) = reqwest::Url::parse(bu) {
            return (
                url.host_str().unwrap_or("127.0.0.1").to_string(),
                url.port_or_known_default().unwrap_or(8085),
            );
        }
    }
    (
        t.get("host")
            .and_then(|v| v.as_str())
            .unwrap_or("127.0.0.1")
            .to_string(),
        t.get("port")
            .and_then(|v| v.as_u64())
            .unwrap_or(9931) as u16,
    )
}

/// One scrape per `tick_ms` (Settings ▸ Tick rate; caller passes the
/// validated config value, bounds re-applied defensively — a 0 here would
/// hot-loop the engine).
///
/// Scrape STRATEGY is decided by the endpoint's ANSWER, not the config
/// label: the config `kind` is only the INITIAL guess; every successful
/// scrape classifies the payload (`classify_models`) and a positive
/// detection that contradicts the strategy switches it immediately — a
/// bare llama-server mislabeled `router` is scraped as what it is, and a
/// host that flips roles between restarts recovers without a config edit.
pub fn spawn(
    target: Value,
    tx: std::sync::mpsc::Sender<AppEvent>,
    stop: Arc<AtomicBool>,
    tick_ms: u32,
    ix: usize,
) -> JoinHandle<()> {
    let tick_ms = crate::config::normalize_tick_ms(tick_ms);
    thread::spawn(move || {
        let scraper = Scraper::new();
        let kind = target
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or("auto")
            .to_string();
        let (host, port) = target_addr(&target);
        let mut strategy = match kind.as_str() {
            "router" => EngineMode::Router,
            // "server" and any explicit "auto"/unknown start as Single:
            // the plainest llama.cpp reading; Router is detected from a
            // status-bearing payload and self-heals on the first tick.
            _ => EngineMode::Single,
        };

        while !stop.load(Ordering::Relaxed) {
            let t0 = Instant::now();

            let mut sample = match strategy {
                EngineMode::Router => scraper
                    .scrape_router(&host, port)
                    .map(SampleData::Router)
                    .ok_or_else(|| ScrapeError("Router unreachable".into())),
                _ => scraper
                    .scrape_server(&host, port)
                    .map(SampleData::Server)
                    .ok_or_else(|| ScrapeError("Server unreachable".into())),
            };
            // self-heal: act only on POSITIVE detections (Unknown never
            // moves the strategy — a cold empty listing is not evidence)
            if let Ok(data) = &sample {
                let (models, slots_ok) = match data {
                    SampleData::Router(rs) => (&rs.models, false),
                    SampleData::Server(ss) => (&ss.models, ss.slots.is_some()),
                };
                let seen = classify_models(models, slots_ok);
                if seen != EngineMode::Unknown && seen != strategy {
                    strategy = seen;
                    sample = match seen {
                        EngineMode::Router => scraper
                            .scrape_router(&host, port)
                            .map(SampleData::Router)
                            .ok_or_else(|| ScrapeError("Router unreachable".into())),
                        _ => scraper
                            .scrape_server(&host, port)
                            .map(SampleData::Server)
                            .ok_or_else(|| ScrapeError("Server unreachable".into())),
                    };
                }
            }

            let _ = tx.send(AppEvent::Sample(ix, sample));

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

#[cfg(test)]
mod tests {
    use super::target_addr;
    use serde_json::json;

    #[test]
    fn addr_reads_base_url_then_host_port() {
        let r = json!({"kind":"router","base_url":"http://10.0.0.2:8085"});
        assert_eq!(target_addr(&r), ("10.0.0.2".into(), 8085));
        let s = json!({"kind":"server","host":"127.0.0.1","port":8080});
        assert_eq!(target_addr(&s), ("127.0.0.1".into(), 8080));
        // auto spelling with base_url only still resolves
        let a = json!({"kind":"auto","base_url":"http://localhost:9931"});
        assert_eq!(target_addr(&a), ("localhost".into(), 9931));
        // nothing sane → the fresh-config default
        assert_eq!(target_addr(&json!({})), ("127.0.0.1".into(), 9931));
    }
}
