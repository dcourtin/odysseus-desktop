//! Mesures du système, lues dans /proc et /sys une fois par seconde :
//! processeur (global et par cœur, fréquence, température), mémoire, carte
//! graphique AMD (charge, mémoire vidéo, température, consommation,
//! ventilateur), disques (place et débit), réseau, processus les plus actifs.

use crate::sysinfo;
use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    time::Instant,
};

/// Durée de l'historique des courbes, en secondes (un point par seconde)
pub const HISTORY: usize = 60;

#[derive(Clone, Default)]
pub struct Cpu {
    pub model: String,
    /// Charge globale et par cœur, 0–1
    pub load: f32,
    pub cores: Vec<f32>,
    pub ghz: Option<f32>,
    pub temp: Option<f32>,
    pub history: VecDeque<f32>,
}

#[derive(Clone, Default)]
pub struct Memory {
    pub total: u64,
    pub used: u64,
    pub cache: u64,
    pub swap_total: u64,
    pub swap_used: u64,
}

#[derive(Clone, Default)]
pub struct Gpu {
    pub name: String,
    pub busy: f32,
    pub vram_used: u64,
    pub vram_total: u64,
    pub temp: Option<f32>,
    pub watts: Option<f32>,
    pub fan_rpm: Option<u32>,
    pub history: VecDeque<f32>,
}

#[derive(Clone)]
pub struct Disk {
    pub label: String,
    pub mount: PathBuf,
    pub total: u64,
    pub free: u64,
}

#[derive(Clone, Default)]
pub struct Net {
    pub interface: String,
    /// Octets par seconde
    pub down: f64,
    pub up: f64,
    pub down_history: VecDeque<f64>,
    pub up_history: VecDeque<f64>,
}

#[derive(Clone)]
pub struct Process {
    pub pid: i32,
    pub name: String,
    /// En pourcentage d'un cœur (comme top : peut dépasser 100)
    pub cpu: f32,
    pub rss: u64,
}

#[derive(Clone, Default)]
pub struct Snapshot {
    pub cpu: Cpu,
    pub memory: Memory,
    pub gpu: Option<Gpu>,
    pub disks: Vec<Disk>,
    /// Débit des disques, octets par seconde
    pub disk_read: f64,
    pub disk_write: f64,
    pub disk_read_history: VecDeque<f64>,
    pub disk_write_history: VecDeque<f64>,
    pub net: Net,
    pub processes: Vec<Process>,
    pub uptime: u64,
}

/// Échantillonneur : garde les compteurs précédents pour calculer les débits.
pub struct Sampler {
    snapshot: Snapshot,
    last: Instant,
    cpu_times: Vec<(u64, u64)>,
    disk_bytes: (u64, u64),
    net_bytes: (u64, u64),
    proc_ticks: HashMap<i32, u64>,
    gpu_dir: Option<PathBuf>,
    clk_tck: f64,
    page: u64,
}

impl Default for Sampler {
    fn default() -> Self {
        Self::new()
    }
}

impl Sampler {
    pub fn new() -> Self {
        let gpu_dir = sysinfo::gpu_device();
        let mut snapshot = Snapshot::default();
        snapshot.cpu.model = sysinfo::cpu_model();
        snapshot.gpu = gpu_dir.as_ref().map(|d| Gpu {
            name: sysinfo::gpu_name(d),
            ..Default::default()
        });
        let mut sampler = Self {
            snapshot,
            last: Instant::now(),
            cpu_times: Vec::new(),
            disk_bytes: disk_bytes(),
            net_bytes: net_bytes().1,
            proc_ticks: HashMap::new(),
            gpu_dir,
            clk_tck: unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as f64,
            page: unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(4096) as u64,
        };
        // Premier passage : les compteurs de référence (débits et charges à 0)
        sampler.sample();
        sampler
    }

    pub fn sample(&mut self) -> Snapshot {
        let dt = self.last.elapsed().as_secs_f64().max(0.2);
        self.last = Instant::now();
        self.sample_cpu();
        self.snapshot.memory = memory();
        self.sample_gpu();
        self.snapshot.disks = disks();
        let (read, written) = disk_bytes();
        self.snapshot.disk_read = read.saturating_sub(self.disk_bytes.0) as f64 / dt;
        self.snapshot.disk_write = written.saturating_sub(self.disk_bytes.1) as f64 / dt;
        self.disk_bytes = (read, written);
        push(
            &mut self.snapshot.disk_read_history,
            self.snapshot.disk_read,
        );
        push(
            &mut self.snapshot.disk_write_history,
            self.snapshot.disk_write,
        );
        let (interface, (rx, tx)) = net_bytes();
        let net = &mut self.snapshot.net;
        net.interface = interface;
        net.down = rx.saturating_sub(self.net_bytes.0) as f64 / dt;
        net.up = tx.saturating_sub(self.net_bytes.1) as f64 / dt;
        push(&mut net.down_history, net.down);
        push(&mut net.up_history, net.up);
        self.net_bytes = (rx, tx);
        self.sample_processes(dt);
        self.snapshot.uptime = read_to_string("/proc/uptime")
            .split('.')
            .next()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        self.snapshot.clone()
    }

    fn sample_cpu(&mut self) {
        let stat = read_to_string("/proc/stat");
        let times: Vec<(u64, u64)> = stat
            .lines()
            .take_while(|l| l.starts_with("cpu"))
            .map(|l| {
                let v: Vec<u64> = l
                    .split_whitespace()
                    .skip(1)
                    .filter_map(|x| x.parse().ok())
                    .collect();
                // inactif = idle + iowait
                let idle = v.get(3).copied().unwrap_or(0) + v.get(4).copied().unwrap_or(0);
                (v.iter().take(8).sum(), idle)
            })
            .collect();
        let loads: Vec<f32> = times
            .iter()
            .enumerate()
            .map(|(i, (total, idle))| {
                let (t0, i0) = self.cpu_times.get(i).copied().unwrap_or((*total, *idle));
                let dt = total.saturating_sub(t0);
                if dt == 0 {
                    0.0
                } else {
                    1.0 - idle.saturating_sub(i0) as f32 / dt as f32
                }
            })
            .collect();
        self.cpu_times = times;
        let cpu = &mut self.snapshot.cpu;
        cpu.load = loads.first().copied().unwrap_or(0.0).clamp(0.0, 1.0);
        cpu.cores = loads.iter().skip(1).map(|l| l.clamp(0.0, 1.0)).collect();
        push(&mut cpu.history, cpu.load);
        // Fréquence moyenne des cœurs (kHz dans sysfs)
        let freqs: Vec<f32> = (0..cpu.cores.len())
            .filter_map(|i| {
                read_to_string(format!(
                    "/sys/devices/system/cpu/cpu{i}/cpufreq/scaling_cur_freq"
                ))
                .trim()
                .parse::<f32>()
                .ok()
            })
            .collect();
        cpu.ghz = (!freqs.is_empty())
            .then(|| freqs.iter().sum::<f32>() / freqs.len() as f32 / 1_000_000.0);
        cpu.temp = hwmon("k10temp").and_then(|h| milli(&h.join("temp1_input")));
    }

    fn sample_gpu(&mut self) {
        let (Some(dir), Some(gpu)) = (self.gpu_dir.as_ref(), self.snapshot.gpu.as_mut()) else {
            return;
        };
        let num = |name: &str| read_to_string(dir.join(name)).trim().parse::<u64>().ok();
        gpu.busy = num("gpu_busy_percent").unwrap_or(0) as f32 / 100.0;
        gpu.vram_used = num("mem_info_vram_used").unwrap_or(0);
        gpu.vram_total = num("mem_info_vram_total").unwrap_or(0);
        push(&mut gpu.history, gpu.busy);
        let hw = std::fs::read_dir(dir.join("hwmon"))
            .ok()
            .and_then(|mut d| d.next())
            .and_then(|e| e.ok())
            .map(|e| e.path());
        if let Some(hw) = hw {
            // Point chaud (junction) si disponible, sinon le bord de la puce
            gpu.temp = milli(&hw.join("temp2_input")).or_else(|| milli(&hw.join("temp1_input")));
            gpu.watts = ["power1_average", "power1_input"].iter().find_map(|f| {
                read_to_string(hw.join(f))
                    .trim()
                    .parse::<f32>()
                    .ok()
                    .map(|uw| uw / 1_000_000.0)
            });
            gpu.fan_rpm = read_to_string(hw.join("fan1_input")).trim().parse().ok();
        }
    }

    fn sample_processes(&mut self, dt: f64) {
        let mut ticks = HashMap::new();
        let mut list = Vec::new();
        let Ok(dir) = std::fs::read_dir("/proc") else {
            return;
        };
        for entry in dir.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|s| s.parse::<i32>().ok())
            else {
                continue;
            };
            let stat = read_to_string(format!("/proc/{pid}/stat"));
            // Le nom est entre parenthèses et peut contenir des espaces
            let (Some(open), Some(close)) = (stat.find('('), stat.rfind(')')) else {
                continue;
            };
            let name = stat[open + 1..close].to_string();
            let fields: Vec<&str> = stat[close + 2..].split_whitespace().collect();
            // Champs 14 et 15 (utime, stime) puis 24 (rss), comptés depuis l'état (3)
            let field = |n: usize| fields.get(n - 3).and_then(|v| v.parse::<u64>().ok());
            let (Some(utime), Some(stime), Some(rss)) = (field(14), field(15), field(24)) else {
                continue;
            };
            // Fils du noyau : pas de mémoire propre, rien à arrêter
            if rss == 0 {
                continue;
            }
            let total = utime + stime;
            ticks.insert(pid, total);
            let Some(before) = self.proc_ticks.get(&pid) else {
                continue;
            };
            let cpu = total.saturating_sub(*before) as f64 / self.clk_tck / dt * 100.0;
            list.push(Process {
                pid,
                name,
                cpu: cpu as f32,
                rss: rss * self.page,
            });
        }
        self.proc_ticks = ticks;
        list.sort_by(|a, b| b.cpu.total_cmp(&a.cpu).then(b.rss.cmp(&a.rss)));
        list.truncate(6);
        self.snapshot.processes = list;
    }
}

fn push<T>(history: &mut VecDeque<T>, value: T) {
    history.push_back(value);
    while history.len() > HISTORY {
        history.pop_front();
    }
}

fn read_to_string(path: impl AsRef<Path>) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// Valeur en millièmes (températures de hwmon) convertie en unités
fn milli(path: &Path) -> Option<f32> {
    read_to_string(path)
        .trim()
        .parse::<f32>()
        .ok()
        .map(|v| v / 1000.0)
}

/// Dossier hwmon d'un pilote (k10temp, amdgpu…)
fn hwmon(name: &str) -> Option<PathBuf> {
    std::fs::read_dir("/sys/class/hwmon")
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| read_to_string(p.join("name")).trim() == name)
}

fn memory() -> Memory {
    let info = read_to_string("/proc/meminfo");
    let kb = |key: &str| {
        info.lines()
            .find_map(|l| l.strip_prefix(key))
            .and_then(|r| r.trim_start_matches(':').split_whitespace().next())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0)
            * 1024
    };
    let total = kb("MemTotal");
    let used = total.saturating_sub(kb("MemAvailable"));
    let cache = (kb("Buffers") + kb("Cached") + kb("SReclaimable")).min(total - used);
    Memory {
        total,
        used,
        cache,
        swap_total: kb("SwapTotal"),
        swap_used: kb("SwapTotal").saturating_sub(kb("SwapFree")),
    }
}

/// Systèmes de fichiers sur un vrai disque (hors petites partitions de démarrage)
fn disks() -> Vec<Disk> {
    let mut list: Vec<Disk> = Vec::new();
    for line in read_to_string("/proc/self/mounts").lines() {
        let mut f = line.split_whitespace();
        let (Some(dev), Some(mount)) = (f.next(), f.next()) else {
            continue;
        };
        if !dev.starts_with("/dev/") || list.iter().any(|d| d.mount.as_os_str() == mount) {
            continue;
        }
        let mount = mount.replace("\\040", " ");
        let Some((total, free)) = sysinfo::statvfs(&mount) else {
            continue;
        };
        if total < 2_000_000_000 {
            continue;
        }
        let label = match mount.as_str() {
            "/" => "Système".to_string(),
            m => Path::new(m)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| m.to_string()),
        };
        list.push(Disk {
            label,
            mount: PathBuf::from(mount),
            total,
            free,
        });
    }
    list
}

/// Octets lus et écrits par les disques physiques (pas leurs partitions)
fn disk_bytes() -> (u64, u64) {
    let physical: Vec<String> = std::fs::read_dir("/sys/block")
        .map(|d| {
            d.flatten()
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| !n.starts_with("loop") && !n.starts_with("ram"))
                .collect()
        })
        .unwrap_or_default();
    read_to_string("/proc/diskstats")
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            let name = f.get(2)?;
            if !physical.iter().any(|p| p == name) {
                return None;
            }
            // Secteurs de 512 octets lus (champ 6) et écrits (champ 10)
            let sectors = |i: usize| f.get(i).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
            Some((sectors(5) * 512, sectors(9) * 512))
        })
        .fold((0, 0), |(r, w), (a, b)| (r + a, w + b))
}

/// Octets reçus et envoyés par les interfaces réseau, et la plus active
fn net_bytes() -> (String, (u64, u64)) {
    let mut busiest = (String::new(), 0u64);
    let mut sum = (0u64, 0u64);
    for line in read_to_string("/proc/net/dev").lines().skip(2) {
        let Some((name, rest)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        if name == "lo" {
            continue;
        }
        let v: Vec<u64> = rest
            .split_whitespace()
            .filter_map(|x| x.parse().ok())
            .collect();
        let (rx, tx) = (
            v.first().copied().unwrap_or(0),
            v.get(8).copied().unwrap_or(0),
        );
        sum = (sum.0 + rx, sum.1 + tx);
        if rx + tx > busiest.1 {
            busiest = (name.to_string(), rx + tx);
        }
    }
    (busiest.0, sum)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_twice() {
        let mut s = Sampler::new();
        std::thread::sleep(std::time::Duration::from_millis(300));
        let snap = s.sample();
        assert!(!snap.cpu.cores.is_empty());
        assert!(snap.memory.total > 0);
        assert!(snap.memory.used <= snap.memory.total);
        assert!(!snap.cpu.model.is_empty());
    }
}
