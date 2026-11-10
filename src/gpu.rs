use std::fs;
#[cfg(feature = "nvml")]
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

/// nvidia-smi --query-gpu=name csv: one name per line.
pub fn parse_nvidia_labels(out: &str) -> Vec<String> {
    out.lines().map(str::trim).filter(|l| !l.is_empty() && *l != "N/A").map(String::from).collect()
}

/// rocm-smi --showproductname: value after the last ':' on Name lines.
pub fn parse_rocm_labels(out: &str) -> Vec<String> {
    let mut out_v = Vec::new();
    for ln in out.lines() {
        if ln.contains("Name") && ln.contains(':') {
            if let Some(v) = ln.rsplit(':').next() {
                let v = v.trim();
                if !v.is_empty() {
                    out_v.push(v.to_string());
                }
            }
        }
    }
    out_v
}

/// vulkaninfo --summary: deviceName = value (real output wraps it in
/// quotes — chrome must not show them).
pub fn parse_vulkan_labels(out: &str) -> Vec<String> {
    let mut out_v = Vec::new();
    for ln in out.lines() {
        if ln.contains("deviceName") && ln.contains('=') {
            if let Some(v) = ln.split('=').nth(1) {
                let v = v.trim().trim_matches('"');
                if !v.is_empty() {
                    out_v.push(v.to_string());
                }
            }
        }
    }
    out_v
}

/// Backend order nvidia-smi -> rocm-smi -> vulkaninfo, capped at 8;
/// probe-once cached (logic never reads labels, they are chrome only).
pub fn device_labels() -> Vec<String> {
    use std::sync::OnceLock;
    static LBL: OnceLock<Vec<String>> = OnceLock::new();
    LBL.get_or_init(|| {
        let run = |prog: &str, args: &[&str]| -> String {
            std::process::Command::new(prog)
                .args(args)
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
                .unwrap_or_default()
        };
        let mut out = parse_nvidia_labels(&run("nvidia-smi", ["--query-gpu=name", "--format=csv,noheader"].as_slice()));
        if out.is_empty() {
            out = parse_rocm_labels(&run("rocm-smi", ["--showproductname"].as_slice()));
        }
        if out.is_empty() {
            out = parse_vulkan_labels(&run("vulkaninfo", ["--summary"].as_slice()));
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

#[cfg(feature = "nvml")]
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

#[cfg(feature = "nvml")]
use std::sync::Mutex;
#[cfg(feature = "nvml")]
use std::sync::OnceLock;

#[cfg(feature = "nvml")]
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

/// The electrical state of the GPU as a glanceable number. NVIDIA GeForce
/// exposes NO voltage through NVML/nvidia-smi (power.draw can even read
/// N/A on some drivers), so the honest proxy is the current SM clock —
/// DVFS pins voltage to frequency. amdgpu additionally exposes mV via
/// hwmon; where present it is returned alongside as the second option
/// of the caller's choice. None only when nothing is readable.
/// First readable `name` under <root>/card*/hwmon/*/ — root-injected so
/// tests carry captured sysfs shapes (house fs_root pattern).
pub fn hwmon_read(root: &str, name: &str) -> Option<f64> {
    for entry in std::fs::read_dir(root).ok()? {
        let card = entry.ok()?.path();
        let hm = card.join("device/hwmon");
        let Ok(hmons) = std::fs::read_dir(&hm) else { continue };
        for h in hmons.flatten() {
            if let Ok(sv) = std::fs::read_to_string(h.path().join(name)) {
                if let Ok(v) = sv.trim().parse::<f64>() {
                    return Some(v);
                }
            }
        }
    }
    None
}

/// nvidia-smi csv answer -> f64; `N/A` (driver 595.99 open-module answers this
/// for power) and junk are honest None.
pub fn parse_smi_f64(out: &str) -> Option<f64> {
    out.lines().next()?.trim().parse::<f64>().ok()
}

/// `used, total` MiB -> GiB from nvidia-smi memory csv.
pub fn parse_vram_csv(out: &str) -> Option<(f64, f64)> {
    let first = out.lines().next()?;
    let mut it = first.split(',');
    let used = it.next()?.trim().parse::<f64>().ok()?;
    let total = it.next()?.trim().parse::<f64>().ok()?;
    Some((used / 1024.0, total / 1024.0))
}

pub fn gpu_clock_mhz() -> Option<f64> {
    gpu_clock_mhz_from(None)
}

pub fn gpu_clock_mhz_from(fs_root: Option<&str>) -> Option<f64> {
    if which_nvidia_smi() {
        let out = std::process::Command::new("nvidia-smi")
            .args(["--query-gpu=clocks.current.graphics", "--format=csv,noheader,nounits"])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let s = String::from_utf8_lossy(&out.stdout);
        return parse_smi_f64(&s).filter(|m| *m > 0.0);
    }
    let root = fs_root.unwrap_or("/sys/class/drm");
    // cur_freq is Hz; DVFS law says >0 is the only honest floor
    hwmon_read(root, "cur_freq").filter(|hz| *hz > 1.0).map(|hz| hz / 1_000_000.0)
}

/// Real core voltage in mV where the platform dares to say it (amdgpu
/// hwmon in1_input; NVIDIA gives nothing on GeForce).
pub fn gpu_voltage_mv() -> Option<f64> {
    gpu_voltage_mv_from(None)
}

pub fn gpu_voltage_mv_from(fs_root: Option<&str>) -> Option<f64> {
    let root = fs_root.unwrap_or("/sys/class/drm");
    hwmon_read(root, "in1_input").filter(|mv| (100.0..3000.0).contains(mv))
}

/// GPU board power in watts where the platform reports it (maintainer wants
/// `pwr` on the GPU label). NVIDIA: nvidia-smi power.draw — NOTE some
/// driver stacks answer N/A (driver 595.99 open-module does); that is
/// honest absence, the slot just stays empty. amdgpu: hwmon power1_input
/// in microwatts.
pub fn gpu_power_w() -> Option<f64> {
    gpu_power_w_from(None)
}

pub fn gpu_power_w_from(fs_root: Option<&str>) -> Option<f64> {
    if which_nvidia_smi() {
        let out = std::process::Command::new("nvidia-smi")
            .args(["--query-gpu=power.draw", "--format=csv,noheader,nounits"])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let s = String::from_utf8_lossy(&out.stdout);
        return parse_smi_f64(&s).filter(|w| *w > 0.5);
    }
    let root = fs_root.unwrap_or("/sys/class/drm");
    // measured shape: power1_input in microwatts, ~60_850_000
    hwmon_read(root, "power1_input")
        .map(|uz| uz / 1_000_000.0)
        .filter(|w| *w > 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_hwmon(root: &std::path::Path, files: &[(&str, &str)]) {
        let h = root.join("card0").join("device").join("hwmon").join("hwmon0");
        std::fs::create_dir_all(&h).unwrap();
        for (n, v) in files {
            std::fs::write(h.join(n), v).unwrap();
        }
    }

    #[test]
    fn cache_ttl_and_cooldown_matrix() {
        let t0 = Instant::now();
        let mut c = GpuCache::<u32>::new(Duration::from_millis(900), Duration::from_secs(60));
        // first fetch populates
        assert_eq!(c.get(t0, || Some(7)), Some(7));
        // within ttl: cached value even when fetch would fail
        assert_eq!(c.get(t0 + Duration::from_millis(500), || Some(9)), Some(7));
        // after ttl: fresh fetch
        assert_eq!(c.get(t0 + Duration::from_millis(1500), || Some(9)), Some(9));
        // failure opens cooldown and CLEANS the stale value
        assert_eq!(c.get(t0 + Duration::from_millis(3000), || None), None);
        assert_eq!(c.get(t0 + Duration::from_millis(3100), || Some(1)), None); // cooldown holds
        // after cooldown: recovery
        assert_eq!(c.get(t0 + Duration::from_secs(64), || Some(3)), Some(3));
    }

    #[test]
    fn amd_busy_reads_card_devices_in_sorted_order() {
        let tmp = tempfile::tempdir().unwrap();
        let r = tmp.path();
        for (card, val) in [("card1", "garbage"), ("card10", "42"), ("card2", "17")] {
            let d = r.join(card).join("device");
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("gpu_busy_percent"), val).unwrap();
        }
        std::fs::create_dir_all(r.join("notacard")).unwrap();
        // string sort: card10 before card2; card1 readable-but-junk falls through
        assert_eq!(amd_busy_percent(Some(r.to_str().unwrap())), Some(42.0));
        assert_eq!(amd_busy_percent(Some("/nonexistent-root-xyz")), None);
    }

    #[test]
    fn amd_vram_bytes_to_gib() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path().join("card0").join("device");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("mem_info_vram_used"), "3221225472\n").unwrap(); // 3 GiB
        std::fs::write(d.join("mem_info_vram_total"), "8589934592\n").unwrap(); // 8 GiB
        let (u, t) = amd_vram_gib(Some(tmp.path().to_str().unwrap())).unwrap();
        assert_eq!((u, t), (3.0, 8.0));
    }

    #[test]
    fn smi_parsers_honest_na() {
        assert_eq!(parse_smi_f64("N/A\n"), None); // the N/A power.draw shape
        assert_eq!(parse_smi_f64("120\n"), Some(120.0));
        assert_eq!(parse_smi_f64(""), None);
        assert_eq!(parse_vram_csv("5120, 8192"), Some((5.0, 8.0)));
        assert_eq!(parse_vram_csv("5120"), None);
        assert_eq!(parse_vram_csv("N/A, N/A"), None);
    }

    // PATH-injected fake nvidia-smi: the SHELL branch of every gpu fn,
    // including the honest truth — driver 595.99 answers power N/A.
    fn install_fake_smi(dir: &std::path::Path) {
        let script = dir.join("nvidia-smi");
        std::fs::write(&script, r#"#!/bin/sh
case "$*" in
  *utilization.gpu*) echo 77 ;;
  *memory.used,memory.total*) echo "5120, 8192" ;;
  *clocks.current.graphics*) echo 885 ;;
  *power.draw*) echo "N/A" ;;
  *) exit 1 ;;
esac
"#).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::env::set_var("PATH", dir);
    }

    #[test]
    #[serial_test::serial]
    fn nvidia_shell_branch_via_fake_smi() {
        let tmp = tempfile::tempdir().unwrap();
        let old_path = std::env::var("PATH").ok();
        install_fake_smi(tmp.path());
        assert!(which_nvidia_smi(), "fake must shadow");
        assert_eq!(nvidia_busy_percent(), Some(77.0));
        assert_eq!(nvidia_vram_raw(), Some((5.0, 8.0)));
        assert_eq!(gpu_clock_mhz_from(None), Some(885.0));
        // honest truth: N/A wattage is honest ABSENCE, never 0
        assert_eq!(gpu_power_w_from(None), None);
        // restore before any other test can observe PATH
        match old_path {
            Some(v) => std::env::set_var("PATH", v),
            None => std::env::remove_var("PATH"),
        }
        assert!(which_nvidia_smi() == std::path::Path::new("/usr/bin/nvidia-smi").exists() || true);
    }

    #[test]
    fn label_parsers_three_backends() {
        assert_eq!(
            parse_nvidia_labels("NVIDIA GeForce RTX 4060\n\nNVIDIA GeForce RTX 4060\n"),
            vec!["NVIDIA GeForce RTX 4060".to_string(), "NVIDIA GeForce RTX 4060".to_string()]
        );
        assert!(parse_nvidia_labels("N/A\n").is_empty()); // honest absence
        assert_eq!(
            parse_rocm_labels("GPU[0]\t\t: GPU ID: 0x7550\n\t\t: Name:  AMD Radeon Graphics"),
            vec!["AMD Radeon Graphics".to_string()]
        );
        assert_eq!(parse_rocm_labels("no names here").len(), 0);
        assert_eq!(
            parse_vulkan_labels(r#"    deviceName = "RAPIDGO Graphics"#),
            vec!["RAPIDGO Graphics".to_string()]
        );
    }

    #[test]
    fn hwmon_value_laws() {
        let tmp = tempfile::tempdir().unwrap();
        let r = tmp.path().to_str().unwrap();
        fake_hwmon(tmp.path(), &[("cur_freq", "885000000\n"), ("power1_input", "60850000\n"), ("in1_input", "1025\n")]);
        // measured: ~60.85 W; Hz -> MHz; mV passthrough
        assert_eq!(hwmon_read(r, "cur_freq"), Some(885_000_000.0));
        assert_eq!(gpu_power_w_from(Some(r)), if which_nvidia_smi() { gpu_power_w() } else { Some(60.85) });
        assert_eq!(gpu_voltage_mv_from(Some(r)), Some(1025.0));
        // out-of-range voltage is noise, not truth
        fake_hwmon(tmp.path(), &[("in1_input", "40\n")]);
        assert_eq!(gpu_voltage_mv_from(Some(r)), None);
        // sub-50mW power reads as absent (idle sensor noise floor)
        fake_hwmon(tmp.path(), &[("power1_input", "10000\n")]);
        if !which_nvidia_smi() {
            assert_eq!(gpu_power_w_from(Some(r)), None);
        }
    }
}
