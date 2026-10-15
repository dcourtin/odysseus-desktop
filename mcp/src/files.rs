//! Lecture des fichiers pour l'agent (catégorie « Lire les fichiers ») : avec
//! les droits de l'utilisateur, et jamais les secrets — clés, mots de passe,
//! jetons, cookies — même si l'utilisateur peut les lire.

use std::path::{Path, PathBuf};

use crate::docs::{self, Kind};

/// Dossiers et fichiers jamais lus ni listés
fn secret(p: &Path) -> bool {
    const DIRS: [&str; 9] = [
        ".ssh",
        ".gnupg",
        ".password-store",
        "keyrings",
        ".mozilla",
        ".claude",
        ".pki",
        ".docker",
        ".aws",
    ];
    const FILES: [&str; 18] = [
        "shadow",
        "gshadow",
        ".netrc",
        ".git-credentials",
        "credentials.json",
        ".credentials.json",
        "credentials.toml",
        ".claude.json",
        ".npmrc",
        ".pypirc",
        // Historiques : un mot de passe tapé par erreur y reste
        ".bash_history",
        ".zsh_history",
        ".python_history",
        ".node_repl_history",
        ".lesshst",
        "environ",
        "kcore",
        "mem",
    ];
    const EXT: [&str; 6] = ["pem", "key", "p12", "pfx", "kdbx", "gpg"];
    let s = p.to_string_lossy();
    if s.starts_with("/dev/") || s.starts_with("/proc/kcore") {
        return true;
    }
    p.components().any(|c| {
        let c = c.as_os_str().to_string_lossy();
        DIRS.contains(&c.as_ref())
            || FILES.contains(&c.as_ref())
            || c.starts_with("id_rsa")
            || c.starts_with("id_ed25519")
            || c.starts_with("id_ecdsa")
            || c.starts_with("shadow-")
    }) || p
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| EXT.contains(&e.to_ascii_lowercase().as_str()))
}

/// Chemin absolu (« ~ » permis), résolu, et pas un secret
fn resolve(path: &str) -> Result<PathBuf, String> {
    let home = std::env::var("HOME").unwrap_or_default();
    let p = if let Some(rest) = path.strip_prefix("~/") {
        PathBuf::from(&home).join(rest)
    } else if path == "~" {
        PathBuf::from(&home)
    } else {
        PathBuf::from(path)
    };
    if !p.is_absolute() {
        return Err(format!("Chemin absolu attendu : {path}"));
    }
    let real = p
        .canonicalize()
        .map_err(|e| format!("{path} : {}", e.kind()))?;
    if secret(&p) || secret(&real) {
        return Err(format!(
            "{path} : refusé, ce fichier peut contenir des secrets (clés, mots de passe, jetons)"
        ));
    }
    Ok(real)
}

pub fn read(
    path: &str,
    part: Option<&str>,
    search: Option<&str>,
    start: usize,
) -> Result<String, String> {
    let p = resolve(path)?;
    if p.is_dir() {
        return list(path, false);
    }
    let text = docs::read_doc(&p).map_err(|e| {
        if e == "Fichier binaire" {
            let size = p.metadata().map(|m| m.len()).unwrap_or(0);
            let kind = std::process::Command::new("file")
                .arg("-b")
                .arg(&p)
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_default();
            format!("{path} : fichier binaire ({kind}, {size} octets), non affiché")
        } else {
            format!("{path} : {e}")
        }
    })?;
    if text.trim().is_empty() {
        return Ok(format!("{path} : fichier vide"));
    }
    Ok(docs::excerpt(&text, Kind::Doc, part, search, start))
}

pub fn list(path: &str, hidden: bool) -> Result<String, String> {
    let p = resolve(path)?;
    let entries = std::fs::read_dir(&p).map_err(|e| format!("{path} : {}", e.kind()))?;
    let mut rows: Vec<(bool, String, String)> = Vec::new();
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !hidden && name.starts_with('.') {
            continue;
        }
        let path = e.path();
        let meta = e.metadata().ok();
        let is_dir = meta.as_ref().is_some_and(|m| m.is_dir());
        let detail = if secret(&path) {
            "secret, non lisible par l'agent".to_string()
        } else if is_dir {
            "dossier".to_string()
        } else {
            let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
            let when = meta
                .and_then(|m| m.modified().ok())
                .map(|t| {
                    chrono::DateTime::<chrono::Local>::from(t)
                        .format("%Y-%m-%d %H:%M")
                        .to_string()
                })
                .unwrap_or_default();
            format!("{size} octets, {when}")
        };
        rows.push((is_dir, name, detail));
    }
    rows.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then(a.1.to_lowercase().cmp(&b.1.to_lowercase()))
    });
    if rows.is_empty() {
        return Ok(format!("{} : dossier vide", p.display()));
    }
    let n = rows.len();
    let mut out = format!("{} ({n} éléments) :\n", p.display());
    for (dir, name, detail) in rows.iter().take(300) {
        out.push_str(&format!(
            "- {name}{} — {detail}\n",
            if *dir { "/" } else { "" }
        ));
    }
    if n > 300 {
        out.push_str(&format!("… et {} autres\n", n - 300));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_refused() {
        for p in [
            "/home/u/.ssh/config",
            "/etc/shadow",
            "/home/u/.gnupg/pubring.kbx",
            "/home/u/cle.pem",
            "/proc/1/environ",
            "/home/u/.claude/.credentials.json",
            "/home/u/.mozilla/firefox/x/logins.json",
            "/home/u/.claude.json",
            "/home/u/.bash_history",
            "/home/u/.cargo/credentials.toml",
        ] {
            assert!(secret(Path::new(p)), "{p}");
        }
        for p in [
            "/etc/fstab",
            "/home/u/notes.md",
            "/proc/cpuinfo",
            "/home/u/keyboard.txt",
        ] {
            assert!(!secret(Path::new(p)), "{p}");
        }
    }
}
