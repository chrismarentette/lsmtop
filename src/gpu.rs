use std::fs;
use std::process::Command;
use std::time::{Duration, Instant};

#[derive(Debug)]
pub struct GpuCache<T> {
    pub last_ok: Option<Instant>,
    pub val: Option<T>,
    pub cooldown_until: Option<Instant>,
    pub ttl_ok: Duration,
    pub ttl_fail: Duration,
}

impl<T: Clone> GpuCache<T> {
    pub fn new(ttl_ok: Duration, ttl_fail: Duration) -> Self {
        Self {
            last_ok: None,
            val: None,
            cooldown_until: None,
            ttl_ok,
            ttl_fail,
        }
    }

    pub fn get<F>(&mut self, now: Instant, fetch: F) -> Option<T>
    where
        F: FnOnce() -> Option<T>,
    {
        if let Some(ok_time) = self.last_ok {
            if now.duration_since(ok_time) < self.ttl_ok {
                return self.val.clone();
            }
        }
        if let Some(cooldown) = self.cooldown_until {
            if now < cooldown {
                return None;
            }
        }

        if let Some(new_val) = fetch() {
            self.val = Some(new_val.clone());
            self.last_ok = Some(now);
            self.cooldown_until = None;
            Some(new_val)
        } else {
            self.val = None;
            self.last_ok = None;
            self.cooldown_until = Some(now + self.ttl_fail);
            None
        }
    }
}

pub fn amd_busy_percent(fs_root: Option<&str>) -> Option<f32> {
    let root = fs_root.unwrap_or("/sys/class/drm");
    if let Ok(entries) = fs::read_dir(root) {
        let mut paths: Vec<_> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with("card"))
            .map(|e| e.path().join("device/gpu_busy_percent"))
            .collect();
        paths.sort();

        for p in paths {
            if let Ok(content) = fs::read_to_string(p) {
                if let Ok(val) = content.trim().parse::<f32>() {
                    return Some(val);
                }
            }
        }
    }
    None
}

pub fn amd_vram_gib(fs_root: Option<&str>) -> Option<(f64, f64)> {
    let root = fs_root.unwrap_or("/sys/class/drm");
    if let Ok(entries) = fs::read_dir(root) {
        let mut dirs: Vec<_> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with("card"))
            .map(|e| e.path().join("device"))
            .collect();
        dirs.sort();

        for d in dirs {
            let p_used = d.join("mem_info_vram_used");
            let p_total = d.join("mem_info_vram_total");
            if let (Ok(u_str), Ok(t_str)) = (fs::read_to_string(p_used), fs::read_to_string(p_total)) {
                if let (Ok(u), Ok(t)) = (u_str.trim().parse::<f64>(), t_str.trim().parse::<f64>()) {
                    // Assuming bytes, converting to GiB
                    return Some((u / 1073741824.0, t / 1073741824.0));
                }
            }
        }
    }
    None
}

#[cfg(feature = "nvml")]
pub fn nvidia_busy_percent() -> Option<f32> {
    let output = Command::new("nvidia-smi")
        .args(["--query-gpu=utilization.gpu", "--format=csv,noheader,nounits"])
        .output()
        .ok()?;
    
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&output.stdout);
    s.lines().next()?.trim().parse::<f32>().ok()
}

#[cfg(not(feature = "nvml"))]
pub fn nvidia_busy_percent() -> Option<f32> {
    None
}

/// 'amdgpu' | 'nvidia' | 'none' .
pub fn gpu_backend() -> &'static str {
    use std::sync::OnceLock;
    static BE: OnceLock<&'static str> = OnceLock::new();
    BE.get_or_init(|| {
        let amd = std::path::Path::new("/sys/class/drm").exists()
            && std::fs::read_dir("/sys/class/drm")
                .map(|rd| {
                    rd.flatten().any(|e| {
                        e.path().join("device").join("gpu_busy_percent").exists()
                    })
                })
                .unwrap_or(false);
        if amd {
            "amdgpu"
        } else if which_nvidia_smi() {
            "nvidia"
        } else {
            "none"
        }
    })
}

fn which_nvidia_smi() -> bool {
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths)
                .any(|d| d.join("nvidia-smi").is_file())
        })
        .unwrap_or(false)
}

/// Best-effort device names for labels ONLY (detect_device_labels: nvidia-smi -> rocm-smi -> vulkaninfo, bounded,
/// probed once). Logic never reads this.
pub fn device_labels() -> Vec<String> {
    use std::sync::OnceLock;
    static LBL: OnceLock<Vec<String>> = OnceLock::new();
    LBL.get_or_init(|| {
        let mut out = Vec::new();
        let run = |prog: &str, args: &[&str]| -> String {
            std::process::Command::new(prog)
                .args(args)
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
                .unwrap_or_default()
        };
        let s = run("nvidia-smi", &["--query-gpu=name", "--format=csv,noheader"]);
        out.extend(s.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from));
        if out.is_empty() {
            let s = run("rocm-smi", &["--showproductname"]);
            for ln in s.lines() {
                if ln.contains("Name") && ln.contains(':') {
                    if let Some(v) = ln.rsplit(':').next() {
                        let v = v.trim();
                        if !v.is_empty() {
                            out.push(v.to_string());
                        }
                    }
                }
            }
        }
        if out.is_empty() {
            let s = run("vulkaninfo", &["--summary"]);
            for ln in s.lines() {
                if ln.contains("deviceName") && ln.contains('=') {
                    if let Some(v) = ln.split('=').nth(1) {
                        let v = v.trim();
                        if !v.is_empty() {
                            out.push(v.to_string());
                        }
                    }
                }
            }
        }
        out.truncate(8);
        out
    })
    .clone()
}

/// (used, total, available) GiB — MemAvailable semantics ().
pub fn mem_gib() -> (f64, f64, f64) {
    use sysinfo::System;
    let mut sys = System::new();
    sys.refresh_memory();
    (
        sys.used_memory() as f64 / 1073741824.0,
        sys.total_memory() as f64 / 1073741824.0,
        sys.available_memory() as f64 / 1073741824.0,
    )
}

static NV_VRAM_CACHE: std::sync::OnceLock<std::sync::Mutex<GpuCache<(f64, f64)>>> =
    std::sync::OnceLock::new();

#[cfg(feature = "nvml")]
fn nvidia_vram_raw() -> Option<(f64, f64)> {
    let out = std::process::Command::new("nvidia-smi")
        .args([
            "--query-gpu=memory.used,memory.total",
            "--format=csv,noheader,nounits",
        ])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    let first = s.lines().next()?;
    let mut it = first.split(',');
    let used = it.next()?.trim().parse::<f64>().ok()?;
    let total = it.next()?.trim().parse::<f64>().ok()?;
    // MiB -> GiB
    Some((used / 1024.0, total / 1024.0))
}

/// VRAM (used, total) GiB: amdgpu sysfs first, then cached nvidia-smi.
pub fn vram_gib() -> Option<(f64, f64)> {
    if let Some(v) = amd_vram_gib(None) {
        return Some(v);
    }
    #[cfg(feature = "nvml")]
    {
        let c = NV_VRAM_CACHE.get_or_init(|| {
            std::sync::Mutex::new(GpuCache::new(
                std::time::Duration::from_millis(900),
                std::time::Duration::from_secs(60),
            ))
        });
        return c
            .lock()
            .unwrap()
            .get(std::time::Instant::now(), nvidia_vram_raw);
    }
    #[cfg(not(feature = "nvml"))]
    {
        None
    }
}

pub fn sysinfo_mem() -> (f64, f64) {
    use sysinfo::System;
    let mut sys = System::new();
    sys.refresh_memory();
    let used = sys.used_memory() as f64 / 1073741824.0;
    let total = sys.total_memory() as f64 / 1073741824.0;
    (used, total)
}

use std::sync::Mutex;
use std::sync::OnceLock;

static NV_BUSY_CACHE: OnceLock<Mutex<GpuCache<f32>>> = OnceLock::new();

#[cfg(feature = "nvml")]
pub fn gpu_busy_percent() -> Option<f32> {
    if let Some(amd) = amd_busy_percent(None) {
        return Some(amd);
    }
    
    let cache = NV_BUSY_CACHE.get_or_init(|| {
        Mutex::new(GpuCache::new(Duration::from_millis(900), Duration::from_secs(60)))
    });
    
    cache.lock().unwrap().get(Instant::now(), nvidia_busy_percent)
}

#[cfg(not(feature = "nvml"))]
pub fn gpu_busy_percent() -> Option<f32> {
    amd_busy_percent(None)
}
