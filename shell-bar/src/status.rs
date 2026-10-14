//! État du système affiché dans la barre, lu sans démon ni D-Bus : réseau
//! (/sys, /proc/net/wireless), Bluetooth connecté (/sys/class/bluetooth),
//! volume (wpctl), « Ne pas déranger » (fichier tenu par odysseus-notify).

use common::theme::icon;

#[derive(Clone, PartialEq, Eq)]
pub struct Status {
    pub network: char,
    pub bluetooth: bool,
    /// Icône du volume (None sans PipeWire)
    pub volume: Option<char>,
    pub dnd: bool,
}

impl Status {
    pub fn read() -> Self {
        Self {
            network: network(),
            bluetooth: bluetooth_connected(),
            volume: volume(),
            dnd: std::env::var_os("HOME").is_some_and(|h| {
                std::path::Path::new(&h)
                    .join(".local/state/odysseus/dnd")
                    .exists()
            }),
        }
    }
}

/// Filaire branché d'abord, sinon Wi-Fi avec sa force, sinon « hors ligne ».
fn network() -> char {
    let Ok(dir) = std::fs::read_dir("/sys/class/net") else {
        return icon::WIFI_OFF;
    };
    let mut wifi = false;
    for entry in dir.flatten() {
        let base = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == "lo" || !base.join("device").exists() {
            continue;
        }
        let up = std::fs::read_to_string(base.join("operstate")).is_ok_and(|s| s.trim() == "up");
        if base.join("wireless").exists() {
            wifi |= up;
        } else if up {
            return icon::ETHERNET;
        }
    }
    if !wifi {
        return icon::WIFI_OFF;
    }
    // /proc/net/wireless : « wlp8s0: 0000   55.  -55.  -256 … » (qualité sur 70)
    let quality = std::fs::read_to_string("/proc/net/wireless")
        .ok()
        .and_then(|text| {
            text.lines().skip(2).find_map(|l| {
                l.split_whitespace()
                    .nth(2)
                    .and_then(|q| q.trim_end_matches('.').parse::<f32>().ok())
            })
        })
        .unwrap_or(70.0);
    match (quality / 70.0 * 100.0) as u32 {
        0..=25 => icon::WIFI_ZERO,
        26..=50 => icon::WIFI_LOW,
        51..=75 => icon::WIFI_HIGH,
        _ => icon::WIFI,
    }
}

/// Une connexion Bluetooth existe : répertoire « hci0:N » sous l'adaptateur.
fn bluetooth_connected() -> bool {
    let Ok(dir) = std::fs::read_dir("/sys/class/bluetooth") else {
        return false;
    };
    dir.flatten()
        .any(|e| e.file_name().to_string_lossy().contains(':'))
}

fn volume() -> Option<char> {
    let out = std::process::Command::new("wpctl")
        .args(["get-volume", "@DEFAULT_AUDIO_SINK@"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let v: f32 = text.split_whitespace().nth(1)?.parse().ok()?;
    Some(if text.contains("MUTED") || v <= 0.0 {
        icon::VOLUME_MUTE
    } else if v < 0.34 {
        icon::VOLUME_0
    } else if v < 0.67 {
        icon::VOLUME_1
    } else {
        icon::VOLUME_2
    })
}
