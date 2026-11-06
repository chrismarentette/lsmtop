pub mod prom;

use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;

pub fn child_ports(models: &Value) -> Vec<(String, u16)> {
    let mut out = Vec::new();
    let data = match models.get("data").and_then(|d| d.as_array()) {
        Some(arr) => arr,
        None => return out,
    };

    for m in data {
        if let Some(status) = m.get("status") {
            if status.get("value").and_then(|v| v.as_str()) != Some("loaded") {
                continue;
            }
            if let Some(args) = status.get("args").and_then(|a| a.as_array()) {
                if let Some(pos) = args.iter().position(|x| x.as_str() == Some("--port")) {
                    if let Some(port_str) = args.get(pos + 1).and_then(|x| x.as_str()) {
                        if let Ok(port) = port_str.parse::<u16>() {
                            if let Some(id) = m.get("id").and_then(|i| i.as_str()) {
                                out.push((id.to_string(), port));
                            }
                        }
                    }
                }
            }
        }
    }
    out
}

#[derive(Clone, PartialEq, Debug)]
pub struct ServerSample {
    pub models: Value,
    pub slots: Option<Value>,
    pub metrics: Option<HashMap<String, f64>>,
}

#[derive(Clone, PartialEq, Debug)]
pub struct RouterSample {
    pub models: Value,
    pub lanes: HashMap<String, LaneSample>,
}

#[derive(Clone, PartialEq, Debug)]
pub struct LaneSample {
    pub slots: Option<Value>,
    pub metrics: Option<HashMap<String, f64>>,
}

pub struct Scraper {
    client: reqwest::blocking::Client,
}

#[allow(clippy::new_without_default)]
impl Scraper {
    pub fn new() -> Self {
        Self {
            client: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(4))
                .build()
                .unwrap(),
        }
    }

    pub fn scrape_server(&self, host: &str, port: u16) -> Option<ServerSample> {
        let base = format!("http://{}:{}", host, port);
        
        let models_resp = self.client.get(format!("{}/models", base))
            .timeout(Duration::from_secs(3))
            .send().ok()?;
        let models = models_resp.json::<Value>().unwrap_or_else(|_| serde_json::json!({}));

        let slots = self.client.get(format!("{}/slots", base))
            .timeout(Duration::from_secs(2))
            .send()
            .and_then(|r| r.json::<Value>())
            .ok();

        let metrics = self.client.get(format!("{}/metrics", base))
            .timeout(Duration::from_secs(4))
            .send()
            .and_then(|r| r.text())
            .map(|t| prom::parse_prom(&t))
            .ok();
            
        // FAILED scrape yields None, NEVER zeros (zeros once read as counter-reset and corrupted rates)

        Some(ServerSample { models, slots, metrics })
    }

    pub fn scrape_router(&self, host: &str, port: u16) -> Option<RouterSample> {
        let base = format!("http://{}:{}", host, port);
        
        let models_resp = self.client.get(format!("{}/models", base))
            .timeout(Duration::from_secs(3))
            .send().ok()?;
        let models = models_resp.json::<Value>().unwrap_or_else(|_| serde_json::json!({}));

        let mut lanes = HashMap::new();
        for (mid, cport) in child_ports(&models) {
            let cbase = format!("http://127.0.0.1:{}", cport);
            
            let slots = self.client.get(format!("{}/slots", cbase))
                .timeout(Duration::from_secs(2))
                .send()
                .and_then(|r| r.json::<Value>())
                .ok();

            let metrics = self.client.get(format!("{}/metrics", cbase))
                .timeout(Duration::from_secs(4))
                .send()
                .and_then(|r| r.text())
                .map(|t| prom::parse_prom(&t))
                .ok();
                
            lanes.insert(mid, LaneSample { slots, metrics });
        }

        Some(RouterSample { models, lanes })
    }
}
pub mod collector;
