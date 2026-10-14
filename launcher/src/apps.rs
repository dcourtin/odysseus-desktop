//! Applications installées (fichiers .desktop) et recherche approximative.

use std::{collections::HashSet, path::PathBuf};

#[derive(Clone, Debug)]
pub struct App {
    pub name: String,
    pub comment: String,
    pub exec: String,
    pub terminal: bool,
    /// Nom d'icône (Icon=) ou chemin
    pub icon: String,
}

/// Dossiers « applications » selon la spécification XDG, du plus prioritaire au moins.
fn application_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let home = std::env::var("HOME").unwrap_or_default();
    let data_home =
        std::env::var("XDG_DATA_HOME").unwrap_or_else(|_| format!("{home}/.local/share"));
    dirs.push(PathBuf::from(data_home).join("applications"));
    let data_dirs =
        std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    dirs.extend(
        data_dirs
            .split(':')
            .filter(|d| !d.is_empty())
            .map(|d| PathBuf::from(d).join("applications")),
    );
    dirs
}

/// Retire les codes de champ (%f, %U…) de la ligne Exec.
fn clean_exec(exec: &str) -> String {
    let mut out = String::new();
    let mut chars = exec.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '%' {
            if let Some('%') = chars.next() {
                out.push('%');
            }
        } else {
            out.push(c);
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn parse(content: &str) -> Option<App> {
    let lang = std::env::var("LANG").unwrap_or_default();
    let short = lang.split(['_', '.']).next().unwrap_or("").to_string();
    let (mut name, mut name_loc, mut comment, mut comment_loc) = (None, None, None, None);
    let (mut exec, mut terminal, mut hidden, mut kind) = (None, false, false, None);
    let mut icon = String::new();
    let mut in_entry = false;
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim().to_string());
        match key {
            "Name" => name = Some(value),
            "Comment" => comment = Some(value),
            "Exec" => exec = Some(value),
            "Icon" => icon = value,
            "Type" => kind = Some(value),
            "Terminal" => terminal = value == "true",
            "NoDisplay" | "Hidden" => hidden |= value == "true",
            k if !short.is_empty() && k == format!("Name[{short}]") => name_loc = Some(value),
            k if !short.is_empty() && k == format!("Comment[{short}]") => comment_loc = Some(value),
            _ => {}
        }
    }
    if hidden || kind.as_deref() != Some("Application") {
        return None;
    }
    Some(App {
        name: name_loc.or(name)?,
        comment: comment_loc.or(comment).unwrap_or_default(),
        exec: clean_exec(&exec?),
        terminal,
        icon,
    })
}

pub fn load() -> Vec<App> {
    let mut seen = HashSet::new();
    let mut apps = Vec::new();
    for dir in application_dirs() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "desktop") {
                continue;
            }
            // Un même identifiant de fichier n'est pris qu'une fois (le plus prioritaire)
            let id = entry.file_name();
            if !seen.insert(id) {
                continue;
            }
            if let Some(app) = std::fs::read_to_string(&path).ok().and_then(|c| parse(&c)) {
                apps.push(app);
            }
        }
    }
    apps.sort_by_key(|a| a.name.to_lowercase());
    apps
}

/// Score de correspondance (plus haut = meilleur), None si aucune.
/// Début du nom > début d'un mot > sous-chaîne > lettres dans l'ordre.
pub fn score(query: &str, app: &App) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let q = query.to_lowercase();
    let name = app.name.to_lowercase();
    if name.starts_with(&q) {
        return Some(1000 - name.len() as i32);
    }
    if name.split([' ', '-', '_']).any(|w| w.starts_with(&q)) {
        return Some(800 - name.len() as i32);
    }
    if let Some(pos) = name.find(&q) {
        return Some(600 - pos as i32);
    }
    let haystack = format!(
        "{name} {} {}",
        app.comment.to_lowercase(),
        app.exec.to_lowercase()
    );
    if haystack.contains(&q) {
        return Some(300);
    }
    // Lettres de la requête dans l'ordre, pénalisées par les trous
    let mut gaps = 0;
    let mut chars = name.chars();
    for qc in q.chars() {
        let mut found = false;
        for c in chars.by_ref() {
            if c == qc {
                found = true;
                break;
            }
            gaps += 1;
        }
        if !found {
            return None;
        }
    }
    Some(100 - gaps)
}
