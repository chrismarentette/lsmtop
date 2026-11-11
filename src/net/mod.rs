pub mod prom;

use crate::app::EngineMode;
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;

/// The ONE engine classifier. A llama-swap router marks every model with
/// status.value; a bare llama.cpp /models is plain OpenAI shape.
/// `slots_ok`: /slots answered on the same host (llama.cpp serves it).
/// Payload decides — a config label never overrides reality.
pub fn classify_models(models: &Value, slots_ok: bool) -> EngineMode {
    let Some(arr) = models
        .get("data")
        .and_then(|d| d.as_array())
        .or_else(|| models.as_array())
    else {
        return EngineMode::Unknown;
    };
    if arr.iter().any(|m| {
        m.get("status")
            .and_then(|s| s.get("value"))
            .and_then(|v| v.as_str())
            .is_some()
    }) {
        return EngineMode::Router;
    }
    if !arr.is_empty() || slots_ok {
        return EngineMode::Single;
    }
    EngineMode::Unknown
}

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

/// Child scrape ENDPOINTS for a router listing: (lane id, host, port).
/// Normally every loaded lane exposes its child port in status.args. A
/// llama-swap running children with `--port 0` (or an equivalent listing
/// without args) proxy-mounts the children on its OWN port — there every
/// loaded lane answers through the router address. Empty when nothing is
/// loaded (an empty listing is not a lane).
pub fn router_child_endpoints(models: &Value, host: &str, port: u16) -> Vec<(String, String, u16)> {
    let kids = child_ports(models);
    if !kids.is_empty() {
        return kids
            .into_iter()
            .map(|(m, p)| (m, "127.0.0.1".to_string(), p))
            .collect();
    }
    let data = match models.get("data").and_then(|d| d.as_array()) {
        Some(arr) => arr,
        None => return Vec::new(),
    };
    for m in data {
        if m.get("status")
            .and_then(|s| s.get("value"))
            .and_then(|v| v.as_str())
            == Some("loaded")
        {
            if let Some(id) = m.get("id").and_then(|i| i.as_str()) {
                return vec![(id.to_string(), host.to_string(), port)];
            }
        }
    }
    Vec::new()
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
        for (mid, chost, cport) in router_child_endpoints(&models, host, port) {
            let cbase = format!("http://{}:{}", chost, cport);
            
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

#[cfg(test)]
mod tests {
    use super::{child_ports, router_child_endpoints};
    use serde_json::json;

    #[test]
    fn router_child_endpoints_honor_args_ports() {
        let m = json!({"data": [
            {"id": "a", "status": {"value": "loaded", "args": ["x", "--port", "9001"]}},
            {"id": "b", "status": {"value": "unloaded"}}
        ]});
        assert_eq!(router_child_endpoints(&m, "10.0.0.9", 8085),
                   vec![("a".to_string(), "127.0.0.1".to_string(), 9001)]);
    }

    #[test]
    fn router_without_args_ports_scrapes_the_proxy_itself() {
        // llama-swap `--port 0`: loaded lane, no args → the router's own
        // host:port is the only endpoint that answers (floodproof law)
        let m = json!({"data": [
            {"id": "filler", "status": {"value": "unloaded"}},
            {"id": "m", "status": {"value": "loaded"}}
        ]});
        assert_eq!(router_child_endpoints(&m, "10.0.0.9", 8085),
                   vec![("m".to_string(), "10.0.0.9".to_string(), 8085)]);
        // nothing loaded → no lanes
        let e = json!({"data": [{"id": "x", "status": {"value": "unloaded"}}]});
        assert!(router_child_endpoints(&e, "h", 1).is_empty());
    }

    // ── engine classifier (Phase 1: single-instance + multi-server) ──
    #[test]
    fn classifier_reads_llama_swap_router() {
        let v = json!({"data":[
            {"id":"m1","status":{"value":"unloaded","args":["--port","0"]}}]});
        assert_eq!(
            super::classify_models(&v, false),
            crate::app::EngineMode::Router
        );
    }
    #[test]
    fn classifier_reads_bare_llamacpp_single() {
        let v = json!({"object":"list","data":[
            {"id":"gemma","object":"model","owned_by":"llamacpp"}]});
        assert_eq!(
            super::classify_models(&v, true),
            crate::app::EngineMode::Single
        );
        // empty data but /slots answers → still single (model loading)
        assert_eq!(
            super::classify_models(&json!({"data":[]}), true),
            crate::app::EngineMode::Single
        );
    }
    #[test]
    fn classifier_reads_junk_as_unknown() {
        assert_eq!(
            super::classify_models(&json!({}), false),
            crate::app::EngineMode::Unknown
        );
    }

    #[test]
    fn only_loaded_models_yield_ports() {
        let v = json!({"data": [
            {"id": "a", "status": {"value": "loaded", "args": ["x", "--port", "8081"]}},
            {"id": "b", "status": {"value": "unloading", "args": ["x", "--port", "8082"]}},
            {"id": "c", "status": {"value": "loaded", "args": ["x"]}},
            {"id": "d", "status": {"value": "loaded", "args": ["--port", "notaport"]}},
            {"id": "e", "status": {"value": "loaded", "args": ["--port", "8085", "--port", "8086"]}},
            {"id": "f"}
        ]});
        // first --port wins; unloadable/no-port/bad-port/no-status all drop
        assert_eq!(child_ports(&v), vec![("a".to_string(), 8081u16), ("e".to_string(), 8085u16)]);
    }

    #[test]
    fn missing_or_malformed_data_is_empty() {
        assert!(child_ports(&json!({})).is_empty());
        assert!(child_ports(&json!({"data": []})).is_empty());
        assert!(child_ports(&json!({"data": "nope"})).is_empty());
        assert!(child_ports(&serde_json::Value::Null).is_empty());
    }
}
