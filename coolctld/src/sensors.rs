//! Direct hwmon/procfs sensor reading — no psutil-equivalent crate, matching the
//! validated approach from the Python reference (Phase 0), for full control over
//! the fallback logic on this specific hardware.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub struct GpuStats {
    pub temp_c: f32,
    pub busy_percent: f32,
    pub clock_ghz: f32,
}

fn read_f32(path: &Path) -> Option<f32> {
    fs::read_to_string(path).ok()?.trim().parse::<f64>().ok().map(|v| v as f32)
}

fn hwmon_name(hwmon_dir: &Path) -> Option<String> {
    fs::read_to_string(hwmon_dir.join("name")).ok().map(|s| s.trim().to_string())
}

fn read_temp_input(hwmon_dir: &Path, index: u32) -> Option<f32> {
    read_f32(&hwmon_dir.join(format!("temp{index}_input"))).map(|milli| milli / 1000.0)
}

/// Which hwmon file provides CPU temp, resolved once at startup.
pub struct CpuTempSource {
    hwmon_dir: PathBuf,
    index: u32,
}

/// Resolve the CPU temp source: `coretemp` -> `k10temp` -> `zenpower` (prefer the
/// `Tdie` label). This machine has no `k10temp` key at all, only `zenpower`.
pub fn resolve_cpu_temp_source() -> Option<CpuTempSource> {
    let entries = fs::read_dir("/sys/class/hwmon").ok()?;

    let mut coretemp = None;
    let mut k10temp = None;
    let mut zenpower = None;

    for entry in entries.flatten() {
        let dir = entry.path();
        match hwmon_name(&dir).as_deref() {
            Some("coretemp") if coretemp.is_none() && dir.join("temp1_input").exists() => {
                coretemp = Some(CpuTempSource { hwmon_dir: dir, index: 1 });
            }
            Some("k10temp") if k10temp.is_none() && dir.join("temp1_input").exists() => {
                k10temp = Some(CpuTempSource { hwmon_dir: dir, index: 1 });
            }
            Some("zenpower") if zenpower.is_none() => {
                let mut index = 1;
                for candidate in 1..=6 {
                    if let Ok(label) = fs::read_to_string(dir.join(format!("temp{candidate}_label"))) {
                        if label.trim() == "Tdie" {
                            index = candidate;
                            break;
                        }
                    }
                }
                zenpower = Some(CpuTempSource { hwmon_dir: dir, index });
            }
            _ => {}
        }
    }

    coretemp.or(k10temp).or(zenpower)
}

/// Average several quick raw reads instead of a single instantaneous sample.
/// A CPU package temp (`Tdie`) can swing fast under bursty per-core boost during
/// gaming — a lone point-sample aliases against whatever instant another tool
/// (e.g. MangoHud) happens to sample at, producing an apparent but spurious gap.
/// Averaging damps that out; total wall time is `(samples - 1) * interval`.
pub fn read_cpu_temp_averaged(source: &CpuTempSource, samples: u32, interval: Duration) -> f32 {
    let mut sum = 0f32;
    let mut count = 0u32;

    for i in 0..samples {
        if let Some(temp) = read_temp_input(&source.hwmon_dir, source.index) {
            sum += temp;
            count += 1;
        }
        if i + 1 < samples {
            std::thread::sleep(interval);
        }
    }

    if count == 0 { 0.0 } else { sum / count as f32 }
}

/// Among `amdgpu` hwmon dirs, the one with a `fan1_input` file is the discrete
/// card (an iGPU has no fan control).
fn find_discrete_amdgpu_hwmon() -> Option<PathBuf> {
    let entries = fs::read_dir("/sys/class/hwmon").ok()?;
    let mut fallback = None;

    for entry in entries.flatten() {
        let dir = entry.path();
        if hwmon_name(&dir).as_deref() != Some("amdgpu") {
            continue;
        }
        if fallback.is_none() {
            fallback = Some(dir.clone());
        }
        if dir.join("fan1_input").exists() {
            return Some(dir);
        }
    }
    fallback
}

/// GPU temp/busy%/clock for the discrete card, via sysfs.
pub fn read_gpu_stats() -> GpuStats {
    let mut stats = GpuStats { temp_c: 0.0, busy_percent: 0.0, clock_ghz: 0.0 };

    let Some(hwmon_dir) = find_discrete_amdgpu_hwmon() else { return stats };

    if let Some(temp) = read_temp_input(&hwmon_dir, 1) {
        stats.temp_c = temp;
    }
    if let Some(hz) = read_f32(&hwmon_dir.join("freq1_input")) {
        stats.clock_ghz = hz / 1_000_000_000.0;
    }
    if let Ok(device_dir) = fs::canonicalize(hwmon_dir.join("device")) {
        if let Some(percent) = read_f32(&device_dir.join("gpu_busy_percent")) {
            stats.busy_percent = percent;
        }
    }

    stats
}

fn read_cpu_jiffies() -> Option<(u64, u64)> {
    let content = fs::read_to_string("/proc/stat").ok()?;
    let line = content.lines().next()?;
    let fields: Vec<u64> = line
        .split_whitespace()
        .skip(1) // skip "cpu" label
        .filter_map(|s| s.parse().ok())
        .collect();
    if fields.len() < 4 {
        return None;
    }
    let idle = fields[3] + fields.get(4).copied().unwrap_or(0); // idle + iowait
    let total: u64 = fields.iter().sum();
    Some((idle, total))
}

/// CPU usage percent, sampled over a 100ms window (same interval psutil used).
pub fn read_cpu_percent() -> f32 {
    let Some((idle1, total1)) = read_cpu_jiffies() else { return 0.0 };
    std::thread::sleep(Duration::from_millis(100));
    let Some((idle2, total2)) = read_cpu_jiffies() else { return 0.0 };

    let idle_delta = idle2.saturating_sub(idle1) as f32;
    let total_delta = total2.saturating_sub(total1) as f32;
    if total_delta <= 0.0 {
        return 0.0;
    }
    (1.0 - idle_delta / total_delta) * 100.0
}

/// Average current CPU clock across cores, in GHz.
pub fn read_cpu_freq_ghz() -> f32 {
    let Ok(entries) = fs::read_dir("/sys/devices/system/cpu") else { return 0.0 };

    let mut sum_khz = 0f64;
    let mut count = 0u32;

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let is_cpu_n = name.starts_with("cpu")
            && name[3..].chars().all(|c| c.is_ascii_digit())
            && !name[3..].is_empty();
        if !is_cpu_n {
            continue;
        }
        let freq_path = entry.path().join("cpufreq/scaling_cur_freq");
        if let Some(khz) = read_f32(&freq_path) {
            sum_khz += khz as f64;
            count += 1;
        }
    }

    if count == 0 {
        return 0.0;
    }
    ((sum_khz / count as f64) / 1_000_000.0) as f32
}
