//! Ce que les Réglages lisent et changent hors d'eux-mêmes : le compositeur
//! (IPC compatible sway : écran, lumière de nuit), systemd-localed (langue du
//! système, par D-Bus), et quelques commandes (presse-papiers, veille).

use std::{
    io::{Read, Write},
    os::unix::{net::UnixStream, process::CommandExt},
    path::PathBuf,
    time::Duration,
};

use dbus::blocking::{Connection, stdintf::org_freedesktop_dbus::Properties};

/// Écran tel que le compositeur le décrit (GET_OUTPUTS)
#[derive(Clone, Default)]
pub struct Screen {
    pub name: String,
    /// (largeur, hauteur, Hz), du plus grand au plus petit
    pub modes: Vec<(i32, i32, i32)>,
    pub current: (i32, i32, i32),
    pub preferred: (i32, i32, i32),
    pub scale: f64,
    pub size_mm: (u32, u32),
}

impl Screen {
    /// Diagonale en pouces, si l'écran donne sa taille
    pub fn inches(&self) -> Option<f64> {
        let (w, h) = (self.size_mm.0 as f64, self.size_mm.1 as f64);
        (w > 0.0 && h > 0.0).then(|| (w * w + h * h).sqrt() / 25.4)
    }

    /// Définitions disponibles, sans doublon de fréquence
    pub fn resolutions(&self) -> Vec<(i32, i32)> {
        let mut list: Vec<(i32, i32)> = self.modes.iter().map(|m| (m.0, m.1)).collect();
        list.dedup();
        list
    }

    /// Fréquences possibles pour une définition, de la plus haute à la plus basse
    pub fn refreshes(&self, w: i32, h: i32) -> Vec<i32> {
        let mut list: Vec<i32> = self
            .modes
            .iter()
            .filter(|m| m.0 == w && m.1 == h)
            .map(|m| m.2)
            .collect();
        list.sort_by(|a, b| b.cmp(a));
        list.dedup();
        list
    }
}

fn ipc_path() -> Option<PathBuf> {
    if let Ok(sock) = std::env::var("SWAYSOCK") {
        return Some(sock.into());
    }
    let dir = std::env::var_os("XDG_RUNTIME_DIR")?;
    let display = std::env::var("WAYLAND_DISPLAY").ok()?;
    Some(PathBuf::from(dir).join(format!("odysseus-ipc.{display}.sock")))
}

/// Un message IPC (protocole i3) et sa réponse
fn ipc(kind: u32, payload: &str) -> Result<String, String> {
    let path = ipc_path().ok_or("compositeur injoignable")?;
    let mut stream = UnixStream::connect(path).map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| e.to_string())?;
    let mut msg = b"i3-ipc".to_vec();
    msg.extend_from_slice(&(payload.len() as u32).to_ne_bytes());
    msg.extend_from_slice(&kind.to_ne_bytes());
    msg.extend_from_slice(payload.as_bytes());
    stream.write_all(&msg).map_err(|e| e.to_string())?;
    let mut header = [0u8; 14];
    stream.read_exact(&mut header).map_err(|e| e.to_string())?;
    let len = u32::from_ne_bytes([header[6], header[7], header[8], header[9]]) as usize;
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body).map_err(|e| e.to_string())?;
    Ok(String::from_utf8_lossy(&body).into_owned())
}

/// Commande au compositeur (« output scale 1.5 », « nightlight auto »…)
pub fn command(cmd: &str) -> Result<(), String> {
    if ipc(0, cmd)?.contains("\"success\":true") {
        Ok(())
    } else {
        Err(format!("commande refusée : {cmd}"))
    }
}

/// Écran affiché (le premier)
pub fn screen() -> Option<Screen> {
    let json: serde_json::Value = serde_json::from_str(&ipc(3, "").ok()?).ok()?;
    let o = json.as_array()?.first()?;
    let mode = |v: &serde_json::Value| -> Option<(i32, i32, i32)> {
        Some((
            v.get("width")?.as_i64()? as i32,
            v.get("height")?.as_i64()? as i32,
            (v.get("refresh")?.as_i64()? as f64 / 1000.0).round() as i32,
        ))
    };
    Some(Screen {
        name: o.get("name")?.as_str()?.to_string(),
        modes: o
            .get("modes")?
            .as_array()?
            .iter()
            .filter_map(mode)
            .collect(),
        current: mode(o.get("current_mode")?)?,
        preferred: o.get("preferred_mode").and_then(mode).unwrap_or_default(),
        scale: o.get("scale").and_then(|v| v.as_f64()).unwrap_or(1.0),
        size_mm: (
            o.get("physical_width")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32,
            o.get("physical_height")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32,
        ),
    })
}

/// Mode de la lumière de nuit enregistré par le compositeur
pub fn night_mode() -> String {
    std::env::var_os("HOME")
        .and_then(|h| {
            std::fs::read_to_string(PathBuf::from(h).join(".local/state/odysseus/nightlight")).ok()
        })
        .map(|s| s.trim().to_string())
        .filter(|s| s == "on" || s == "auto")
        .unwrap_or_else(|| "off".into())
}

// --- Langue du système (systemd-localed) -------------------------------------

const LOCALED: &str = "org.freedesktop.locale1";

/// Langues installées (« fr_FR.UTF-8 »…), hors C
pub fn locales() -> Vec<String> {
    let out = std::process::Command::new("localectl")
        .arg("list-locales")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    let mut list = out
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("C.") && *l != "C" && *l != "POSIX")
        .map(String::from)
        .collect::<Vec<_>>();
    // Par nom affiché : « Allemand », « Anglais »… « Français »
    list.sort_by_key(|l| locale_name(l));
    list
}

/// Langue du système (LANG de /etc/locale.conf)
pub fn current_locale() -> Option<String> {
    let conn = Connection::new_system().ok()?;
    let proxy = conn.with_proxy(LOCALED, "/org/freedesktop/locale1", Duration::from_secs(3));
    let list: Vec<String> = proxy.get(LOCALED, "Locale").ok()?;
    list.iter()
        .find_map(|l| l.strip_prefix("LANG=").map(String::from))
}

/// Change la langue du système (autorisé par polkit pour la session locale)
pub fn set_locale(lang: &str) -> Result<(), String> {
    let conn = Connection::new_system().map_err(|e| e.to_string())?;
    let proxy = conn.with_proxy(LOCALED, "/org/freedesktop/locale1", Duration::from_secs(25));
    proxy
        .method_call::<(), _, _, _>(LOCALED, "SetLocale", (vec![format!("LANG={lang}")], false))
        .map_err(|e| e.message().unwrap_or("refusé").to_string())
}

/// Nom d'une langue, en français : « fr_FR.UTF-8 » → « Français (France) »
pub fn locale_name(locale: &str) -> String {
    let code = locale.split('.').next().unwrap_or(locale);
    let (lang, region) = code.split_once('_').unwrap_or((code, ""));
    let lang_name = match lang {
        "fr" => "Français",
        "en" => "Anglais",
        "de" => "Allemand",
        "es" => "Espagnol",
        "it" => "Italien",
        "pt" => "Portugais",
        "nl" => "Néerlandais",
        "cs" => "Tchèque",
        "pl" => "Polonais",
        "ru" => "Russe",
        "ja" => "Japonais",
        "zh" => "Chinois",
        "ko" => "Coréen",
        "fa" => "Persan",
        "is" => "Islandais",
        "el" => "Grec",
        "sv" => "Suédois",
        "da" => "Danois",
        "fi" => "Finnois",
        "nb" | "no" => "Norvégien",
        "tr" => "Turc",
        "ar" => "Arabe",
        "he" => "Hébreu",
        other => other,
    };
    let region_name = match region {
        "" => return lang_name.to_string(),
        "FR" => "France",
        "BE" => "Belgique",
        "CH" => "Suisse",
        "CA" => "Canada",
        "US" => "États-Unis",
        "GB" => "Royaume-Uni",
        "DE" => "Allemagne",
        "AT" => "Autriche",
        "ES" => "Espagne",
        "IT" => "Italie",
        "PT" => "Portugal",
        "BR" => "Brésil",
        "CZ" => "Tchéquie",
        "IR" => "Iran",
        "IS" => "Islande",
        "JP" => "Japon",
        "CN" => "Chine",
        "TW" => "Taïwan",
        "KR" => "Corée",
        "RU" => "Russie",
        "NL" => "Pays-Bas",
        "PL" => "Pologne",
        "GR" => "Grèce",
        other => other,
    };
    format!("{lang_name} ({region_name})")
}

// --- Divers -------------------------------------------------------------------

/// Lance un programme d'odysseus installé à côté des Réglages
pub fn spawn_sibling(name: &str, args: &[&str]) {
    let Some(exe) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(name)))
    else {
        return;
    };
    if let Ok(mut child) = std::process::Command::new(exe)
        .args(args)
        .process_group(0)
        .spawn()
    {
        std::thread::spawn(move || child.wait());
    }
}

/// Texte dans le presse-papiers
pub fn copy_text(text: &str) -> Result<(), String> {
    let mut child = std::process::Command::new("wl-copy")
        .arg("--")
        .arg(text)
        .process_group(0)
        .spawn()
        .map_err(|e| format!("presse-papiers indisponible : {e}"))?;
    std::thread::spawn(move || child.wait());
    Ok(())
}

/// Verrouille puis met le système en veille (comme le centre de contrôle)
pub fn suspend_now() {
    spawn_sibling("odysseus-lock", &[]);
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_millis(800));
        let _ = std::process::Command::new("systemctl")
            .arg("suspend")
            .status();
    });
}

/// Nom complet de l'utilisateur (champ GECOS de /etc/passwd), sinon son identifiant
pub fn user_name() -> (String, String) {
    let login = std::env::var("USER").unwrap_or_else(|_| "utilisateur".into());
    let full = std::fs::read_to_string("/etc/passwd")
        .unwrap_or_default()
        .lines()
        .find(|l| l.starts_with(&format!("{login}:")))
        .and_then(|l| l.split(':').nth(4))
        .map(|g| g.split(',').next().unwrap_or("").trim().to_string())
        .filter(|g| !g.is_empty())
        .unwrap_or_else(|| login.clone());
    (full, login)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(locale_name("fr_FR.UTF-8"), "Français (France)");
        assert_eq!(locale_name("en_US.UTF-8"), "Anglais (États-Unis)");
        let s = Screen {
            modes: vec![(3840, 2160, 60), (3840, 2160, 30), (2560, 1440, 60)],
            ..Default::default()
        };
        assert_eq!(s.resolutions(), vec![(3840, 2160), (2560, 1440)]);
        assert_eq!(s.refreshes(3840, 2160), vec![60, 30]);
    }
}
