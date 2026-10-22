//! Ce que l'utilisateur avait sous les yeux en appelant l'Assistant : la
//! fenêtre active, le texte surligné (sélection primaire de Wayland, dans
//! n'importe quelle application), le presse-papiers, le dossier ouvert dans
//! Fichiers. Rassemblé à l'ouverture du panneau et montré en pastilles que
//! l'on retire d'un clic : l'agent ne reçoit, avec la question, que ce qui
//! est resté affiché. Rien n'est gardé d'une fois sur l'autre.

use std::{path::PathBuf, process::Command};

use serde_json::Value;

/// Au-delà, le texte n'est pas transmis en entier
const MAX_TEXT: usize = 4000;
/// Longueur de l'aperçu dans une pastille
const PREVIEW: usize = 32;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    Window,
    Selection,
    Clipboard,
    Folder,
    Notification,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Window => "fenêtre",
            Kind::Selection => "sélection",
            Kind::Clipboard => "presse-papiers",
            Kind::Folder => "dossier",
            Kind::Notification => "notification",
        }
    }
}

/// Une notification dont on vient (bouton « Demander » d'odysseus-notify) :
/// « Application : titre — texte »
pub fn notification_chip(text: &str) -> Chip {
    Chip {
        kind: Kind::Notification,
        label: format!("Notification : {}", preview(text)),
        text: format!("Notification reçue : {}", bounded(text)),
        dir: None,
    }
}

/// Une pastille de contexte : ce qu'elle montre, ce que l'agent reçoit
#[derive(Clone, Debug)]
pub struct Chip {
    pub kind: Kind,
    pub label: String,
    pub text: String,
    /// Dossier que l'agent peut lire directement (Claude Code : --add-dir)
    pub dir: Option<PathBuf>,
}

/// La fenêtre active, telle que le compositeur la voit (GET_WINDOWS)
#[derive(Clone, Debug, Default)]
pub struct Window {
    pub id: u64,
    pub app_id: String,
    /// Nom lisible de l'application (fichier .desktop), sinon l'identifiant
    pub name: String,
    pub title: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
}

pub fn focused_window() -> Option<Window> {
    let json = common::ipc::request(common::ipc::GET_WINDOWS, "").ok()?;
    let list: Vec<Value> = serde_json::from_str(&json).ok()?;
    let w = list.iter().find(|w| w["focused"].as_bool() == Some(true))?;
    let app_id = w["app_id"].as_str().unwrap_or("").to_string();
    Some(Window {
        id: w["id"].as_u64()?,
        name: app_name(&app_id),
        app_id,
        title: w["title"].as_str().unwrap_or("").to_string(),
        x: w["x"].as_i64().unwrap_or(0) as i32,
        y: w["y"].as_i64().unwrap_or(0) as i32,
        width: w["width"].as_i64().unwrap_or(0) as i32,
    })
}

/// Nom lisible d'une application d'après son identifiant de fenêtre
fn app_name(app_id: &str) -> String {
    if app_id.is_empty() {
        return "application".into();
    }
    let key = app_id.to_lowercase();
    common::apps::load()
        .into_iter()
        .find(|a| {
            a.id.to_lowercase() == key
                || a.wm_class.to_lowercase() == key
                || key.ends_with(&format!(".{}", a.id.to_lowercase()))
        })
        .map(|a| a.name)
        .unwrap_or_else(|| app_id.to_string())
}

/// Texte du presse-papiers ou de la sélection primaire, sans attendre une
/// application qui ne répond pas
fn paste(primary: bool) -> Option<String> {
    let mut cmd = Command::new("timeout");
    cmd.args(["1", "wl-paste", "--no-newline", "--type", "text/plain"]);
    if primary {
        cmd.arg("--primary");
    }
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    let t = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!t.is_empty()).then_some(t)
}

/// Les premiers mots, sur une ligne
fn preview(t: &str) -> String {
    let one: String = t.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() > PREVIEW {
        format!(
            "{}…",
            one.chars().take(PREVIEW).collect::<String>().trim_end()
        )
    } else {
        one
    }
}

/// Le texte tel qu'il part : entier, ou le début avec la mention de la coupe
fn bounded(t: &str) -> String {
    let n = t.chars().count();
    if n <= MAX_TEXT {
        return t.to_string();
    }
    format!(
        "{}\n[… {} caractères non transmis …]",
        t.chars().take(MAX_TEXT).collect::<String>(),
        n - MAX_TEXT
    )
}

/// Chemin abrégé avec « ~ »
fn tilde(p: &str) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    match p.strip_prefix(&home) {
        Some(rest) if !home.is_empty() => format!("~{rest}"),
        _ => p.to_string(),
    }
}

/// Le dossier ouvert dans Fichiers et sa sélection, s'il est au premier plan
fn folder_chip() -> Option<Chip> {
    let display = std::env::var("WAYLAND_DISPLAY").unwrap_or_default();
    let display = display.rsplit('/').next().unwrap_or("").to_string();
    let path = std::env::var_os("XDG_RUNTIME_DIR")
        .map(|d| PathBuf::from(d).join(format!("odysseus-files.{display}.json")))?;
    let state: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    let dir = state["dossier"]
        .as_str()
        .filter(|d| !d.is_empty())?
        .to_string();
    let selection: Vec<String> = state["selection"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();
    let mut label = format!("Dossier : {}", tilde(&dir));
    let mut text = format!("Dossier ouvert dans Fichiers : {dir}");
    match selection.len() {
        0 => {}
        1 => {
            label.push_str(" · 1 élément");
            text.push_str(&format!("\nÉlément sélectionné : {}", selection[0]));
        }
        n => {
            label.push_str(&format!(" · {n} éléments"));
            text.push_str(&format!(
                "\nÉléments sélectionnés ({n}) : {}",
                selection.join(", ")
            ));
        }
    }
    Some(Chip {
        kind: Kind::Folder,
        label,
        text,
        dir: Some(PathBuf::from(dir)),
    })
}

/// Tout ce qui se voit à l'ouverture, dans l'ordre des pastilles
pub fn gather(window: Option<&Window>) -> Vec<Chip> {
    let mut chips = Vec::new();
    if let Some(w) = window {
        let app = &w.name;
        let label = if w.title.is_empty() {
            format!("Fenêtre : {app}")
        } else {
            format!("Fenêtre : {app} · {}", preview(&w.title))
        };
        chips.push(Chip {
            kind: Kind::Window,
            label,
            text: format!("Fenêtre active : {app} — « {} »", w.title),
            dir: None,
        });
        if w.app_id == "odysseus-files"
            && let Some(chip) = folder_chip()
        {
            chips.push(chip);
        }
    }
    let selection = paste(true);
    if let Some(sel) = &selection {
        chips.push(Chip {
            kind: Kind::Selection,
            label: format!("Sélection : « {} »", preview(sel)),
            text: format!("Texte sélectionné (surligné) :\n« {} »", bounded(sel)),
            dir: None,
        });
    }
    if let Some(clip) = paste(false)
        && selection.as_deref() != Some(clip.as_str())
    {
        chips.push(Chip {
            kind: Kind::Clipboard,
            label: format!("Presse-papiers : « {} »", preview(&clip)),
            text: format!("Presse-papiers :\n« {} »", bounded(&clip)),
            dir: None,
        });
    }
    chips
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn previews_and_bounds() {
        assert_eq!(preview("  un\n deux   trois "), "un deux trois");
        let long = "mot ".repeat(20);
        let p = preview(&long);
        assert!(p.ends_with('…') && p.chars().count() <= PREVIEW + 1);
        let huge = "x".repeat(MAX_TEXT + 10);
        assert!(bounded(&huge).contains("10 caractères non transmis"));
        assert_eq!(bounded("court"), "court");
    }
}
