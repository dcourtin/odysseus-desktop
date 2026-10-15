//! Fichiers livrés avec odysseus (polices…), où qu'il soit installé :
//!
//! 1. `$ODYSSEUS_DATA` s'il est défini ;
//! 2. à côté des binaires : `<préfixe>/share/odysseus` (installation dans
//!    `<préfixe>/bin` ou `<préfixe>/lib/odysseus`), ou `data/` du dépôt en
//!    développement (binaires dans `target/release`) ;
//! 3. `/usr/local/share/odysseus`, puis `/usr/share/odysseus`.

use std::path::{Path, PathBuf};

/// Dossiers de données, du plus prioritaire au moins
pub fn dirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(d) = std::env::var_os("ODYSSEUS_DATA") {
        out.push(PathBuf::from(d));
    }
    if let Some(bin) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
    {
        for up in [1, 2] {
            if let Some(prefix) = bin.ancestors().nth(up) {
                out.push(prefix.join("share/odysseus"));
            }
        }
        // Dépôt : <dépôt>/target/release/<binaire>
        if let Some(repo) = bin.ancestors().nth(2) {
            out.push(repo.join("data"));
        }
    }
    out.push(PathBuf::from("/usr/local/share/odysseus"));
    out.push(PathBuf::from("/usr/share/odysseus"));
    out
}

/// Premier fichier `rel` (« fonts/Inter-Medium.ttf ») trouvé dans les dossiers de données
pub fn find(rel: &str) -> Option<PathBuf> {
    dirs()
        .into_iter()
        .map(|d| d.join(rel))
        .find(|p| p.is_file())
}

/// Police : chemin absolu tel quel ; sinon nom de fichier cherché dans les
/// polices d'odysseus, puis dans celles du système (/usr/share/fonts…)
pub fn font(name: &str) -> PathBuf {
    if name.starts_with('/') {
        return PathBuf::from(name);
    }
    if let Some(p) = find(&format!("fonts/{name}")) {
        return p;
    }
    let roots = [
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share/fonts")),
        Some(PathBuf::from("/usr/local/share/fonts")),
        Some(PathBuf::from("/usr/share/fonts")),
    ];
    roots
        .into_iter()
        .flatten()
        .find_map(|r| search(&r, name, 4))
        .unwrap_or_else(|| PathBuf::from(name))
}

fn search(dir: &Path, name: &str, depth: u32) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut subdirs = Vec::new();
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            subdirs.push(p);
        } else if e.file_name() == name {
            return Some(p);
        }
    }
    if depth == 0 {
        return None;
    }
    subdirs
        .into_iter()
        .find_map(|d| search(&d, name, depth - 1))
}

#[cfg(test)]
mod tests {
    #[test]
    fn absolute_paths_kept() {
        assert_eq!(
            super::font("/x/y.ttf"),
            std::path::PathBuf::from("/x/y.ttf")
        );
    }
}
