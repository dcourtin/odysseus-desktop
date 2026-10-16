//! Petites actions à la Finder : dupliquer (Ctrl+D), compresser en .zip,
//! aller à un chemin tapé (Ctrl+L, Tab complète).

use std::path::{Path, PathBuf};

use smithay_client_toolkit::seat::keyboard::{KeyEvent, Keysym};

use crate::{
    Files,
    clip::{self, Pasted},
    field::{Field, Outcome},
};

impl Files {
    /// Ctrl+D : une copie de chaque élément à côté de lui (« nom copie »)
    pub fn duplicate(&mut self) {
        if self.in_trash() {
            return;
        }
        let paths = self.selected_paths();
        if paths.is_empty() {
            return;
        }
        let dir = self.browser.cwd.clone();
        let tx = self.pastes.clone();
        self.browser.message = Some("Copie…".into());
        self.draw();
        std::thread::spawn(move || {
            let _ = tx.send(clip::paste_into(&paths, false, &dir, "dupliqué"));
        });
    }

    /// La sélection dans une archive .zip, à côté : « nom.zip » pour un seul
    /// élément, « Archive.zip » pour plusieurs (comme le Finder)
    pub fn compress(&mut self) {
        if self.in_trash() {
            return;
        }
        let entries: Vec<(String, bool)> = self
            .browser
            .selection()
            .iter()
            .map(|e| (e.name.clone(), e.is_dir))
            .collect();
        let name = match entries.as_slice() {
            [] => return,
            [(one, _)] => format!("{one}.zip"),
            _ => "Archive.zip".into(),
        };
        let dir = self.browser.cwd.clone();
        let dest = clip::copy_name(&dir, &name, false);
        let tx = self.pastes.clone();
        self.browser.message = Some("Compression…".into());
        self.draw();
        std::thread::spawn(move || {
            let _ = tx.send(zip(&dir, &entries, &dest));
        });
    }

    // --- Aller à un chemin (Ctrl+L) ----------------------------------------------

    pub fn start_goto(&mut self) {
        let mut text = self.browser.cwd.to_string_lossy().into_owned();
        if !text.ends_with('/') {
            text.push('/');
        }
        // « ~ » à la place du dossier personnel, plus court à lire et à taper
        if let Some(home) = std::env::var_os("HOME").map(PathBuf::from)
            && let Ok(rest) = Path::new(&text).strip_prefix(&home)
        {
            text = format!("~/{}", rest.to_string_lossy());
            if !text.ends_with('/') {
                text.push('/');
            }
        }
        let mut field = Field::new(&text);
        field.select(0, text.len());
        self.goto = Some(field);
        self.menu = None;
        self.draw();
    }

    /// Clavier pendant la saisie d'un chemin ; vrai si la touche a servi
    pub fn goto_key(&mut self, event: &KeyEvent) -> bool {
        let (ctrl, shift) = (self.modifiers.ctrl, self.modifiers.shift);
        let Some(field) = self.goto.as_mut() else {
            return false;
        };
        if event.keysym == Keysym::Tab {
            let text = field.text.clone();
            if let Some(done) = complete(&text, &self.browser.cwd) {
                field.set(&done);
            }
            self.draw();
            return true;
        }
        match field.key(event, ctrl, shift) {
            Outcome::Submit => {
                let text = field.text.clone();
                self.goto = None;
                self.go_to(&text);
            }
            Outcome::Cancel => {
                self.goto = None;
                self.draw();
            }
            _ => self.draw(),
        }
        true
    }

    fn go_to(&mut self, text: &str) {
        let path = expand(text, &self.browser.cwd);
        if path.is_dir() {
            self.browser.open_dir(&path);
        } else if path.exists()
            && let Some(parent) = path.parent()
        {
            // Un fichier : son dossier, et lui sélectionné
            self.browser.open_dir(parent);
            if let Some(i) = self.browser.entries.iter().position(|e| e.path == path) {
                self.browser.select(i);
            }
        } else {
            self.browser.message = Some(format!("« {text} » introuvable"));
        }
        self.navigated();
    }
}

/// « ~ », chemins relatifs au dossier affiché, « .. »
fn expand(text: &str, cwd: &Path) -> PathBuf {
    let text = text.trim();
    let path = match text.strip_prefix('~') {
        Some(rest) => {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default();
            home.join(rest.trim_start_matches('/'))
        }
        None => cwd.join(text),
    };
    // Normalise « . » et « .. » sans suivre les liens
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// Tab : complète le dernier segment (le préfixe commun des noms qui
/// commencent ainsi ; « / » ajouté quand c'est un dossier unique)
fn complete(text: &str, cwd: &Path) -> Option<String> {
    let (head, prefix) = match text.rfind('/') {
        Some(i) => (&text[..=i], &text[i + 1..]),
        None => ("", text),
    };
    let dir = expand(if head.is_empty() { "." } else { head }, cwd);
    let mut names: Vec<(String, bool)> = std::fs::read_dir(&dir)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let hidden_ok = prefix.starts_with('.') || !name.starts_with('.');
            (hidden_ok && name.starts_with(prefix)).then(|| (name, e.path().is_dir()))
        })
        .collect();
    names.sort();
    let (first, is_dir) = names.first()?.clone();
    if names.len() == 1 {
        return Some(format!("{head}{first}{}", if is_dir { "/" } else { "" }));
    }
    // Préfixe commun
    let mut common = first.clone();
    for (name, _) in &names[1..] {
        while !name.starts_with(&common) {
            common.pop();
        }
    }
    (common.len() > prefix.len()).then(|| format!("{head}{common}"))
}

/// zip -r, depuis le dossier (chemins relatifs dans l'archive)
fn zip(dir: &Path, entries: &[(String, bool)], dest: &Path) -> Pasted {
    let out = std::process::Command::new("zip")
        .args(["-r", "-q", "-y"])
        .arg(dest)
        .arg("--")
        .args(entries.iter().map(|(n, _)| n))
        .current_dir(dir)
        .output();
    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let message = match out {
        Ok(o) if o.status.success() => format!("« {name} » créé"),
        Ok(o) => format!(
            "Compression impossible : {}",
            String::from_utf8_lossy(&o.stderr).trim()
        ),
        Err(e) => format!("Compression impossible (zip) : {e}"),
    };
    let created = if dest.exists() {
        vec![dest.to_path_buf()]
    } else {
        Vec::new()
    };
    Pasted {
        dir: dir.to_path_buf(),
        op: Some(crate::undo::Op::Created(created.clone())),
        created,
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_paths() {
        let cwd = Path::new("/home/a/docs");
        assert_eq!(expand("../img", cwd), PathBuf::from("/home/a/img"));
        assert_eq!(expand("/etc/./x", cwd), PathBuf::from("/etc/x"));
        assert_eq!(expand("sous", cwd), PathBuf::from("/home/a/docs/sous"));
    }

    #[test]
    fn completion() {
        let dir = std::env::temp_dir().join(format!("odys-complete-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Projets")).unwrap();
        std::fs::write(dir.join("photo-1.png"), "").unwrap();
        std::fs::write(dir.join("photo-2.png"), "").unwrap();
        assert_eq!(complete("Pro", &dir).as_deref(), Some("Projets/"));
        assert_eq!(complete("ph", &dir).as_deref(), Some("photo-"));
        assert_eq!(complete("photo-", &dir), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
