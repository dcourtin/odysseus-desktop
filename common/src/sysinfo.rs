//! Informations sur la machine, partagées par le moniteur et les Réglages :
//! modèle du processeur, carte graphique, place sur un disque.

use std::{
    ffi::CString,
    path::{Path, PathBuf},
};

fn read_to_string(path: impl AsRef<Path>) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// « AMD Ryzen 5 8400F 6-Core Processor » → « Ryzen 5 8400F »
pub fn cpu_model() -> String {
    let info = read_to_string("/proc/cpuinfo");
    let model = info
        .lines()
        .find_map(|l| {
            l.strip_prefix("model name")
                .map(|r| r.trim_start_matches([' ', '\t', ':']))
        })
        .unwrap_or("Processeur");
    model
        .split_whitespace()
        .filter(|w| !matches!(*w, "AMD" | "Intel(R)" | "Core(TM)" | "Processor" | "CPU"))
        .filter(|w| !w.ends_with("-Core"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Carte graphique : premier périphérique DRM qui donne sa charge (amdgpu)
pub fn gpu_device() -> Option<PathBuf> {
    let mut cards: Vec<PathBuf> = std::fs::read_dir("/sys/class/drm")
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("card") && !n.contains('-'))
        })
        .map(|p| p.join("device"))
        .filter(|d| d.join("gpu_busy_percent").exists())
        .collect();
    cards.sort();
    cards.into_iter().next()
}

/// Nom commercial d'après la base PCI : « Navi 44 [Radeon RX 9060 XT] » → « Radeon RX 9060 XT »
pub fn gpu_name(dir: &Path) -> String {
    let id = |f: &str| {
        read_to_string(dir.join(f))
            .trim()
            .trim_start_matches("0x")
            .to_string()
    };
    let (vendor, device) = (id("vendor"), id("device"));
    let ids = read_to_string("/usr/share/hwdata/pci.ids");
    let mut in_vendor = false;
    for line in ids.lines() {
        if !line.starts_with('\t') && !line.starts_with('#') {
            in_vendor = line.starts_with(&vendor);
            continue;
        }
        if in_vendor
            && let Some(rest) = line.strip_prefix('\t')
            && !rest.starts_with('\t')
            && rest.starts_with(&device)
        {
            let name = rest[device.len()..].trim();
            return match (name.find('['), name.rfind(']')) {
                (Some(a), Some(b)) if b > a => name[a + 1..b].to_string(),
                _ => name.to_string(),
            };
        }
    }
    "Carte graphique".into()
}

pub fn statvfs(path: &str) -> Option<(u64, u64)> {
    let c = CString::new(path).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    let block = st.f_frsize as u64;
    Some((st.f_blocks as u64 * block, st.f_bavail as u64 * block))
}

/// Nom de la machine
pub fn hostname() -> String {
    read_to_string("/proc/sys/kernel/hostname")
        .trim()
        .to_string()
}

/// « Linux From Scratch 13.1-systemd » (PRETTY_NAME de /etc/os-release)
pub fn os_name() -> String {
    read_to_string("/etc/os-release")
        .lines()
        .find_map(|l| l.strip_prefix("PRETTY_NAME="))
        .map(|v| v.trim_matches('"').to_string())
        .unwrap_or_else(|| "Linux".into())
}

/// Version du noyau
pub fn kernel() -> String {
    read_to_string("/proc/sys/kernel/osrelease")
        .trim()
        .to_string()
}

/// Mémoire vive totale, en octets
pub fn memory_total() -> u64 {
    read_to_string("/proc/meminfo")
        .lines()
        .find_map(|l| l.strip_prefix("MemTotal:"))
        .and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok())
        .unwrap_or(0)
        * 1024
}

/// Nombre de cœurs logiques
pub fn cpu_threads() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get())
}
