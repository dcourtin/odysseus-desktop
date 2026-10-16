//! Copier, couper, coller des fichiers par le presse-papiers : les autres
//! applications reçoivent une liste d'URI (et le format de GNOME, qui dit
//! « copier » ou « couper »), un terminal les chemins ; Fichiers colle aussi ce
//! que d'autres explorateurs ont copié.

use std::{
    io::{Read, Write},
    os::fd::OwnedFd,
    path::{Path, PathBuf},
};

use smithay_client_toolkit::data_device_manager::{WritePipe, data_source::CopyPasteSource};

use crate::{Files, dnd, info};

const URI_LIST: &str = "text/uri-list";
const GNOME: &str = "x-special/gnome-copied-files";
/// Types annoncés, du plus riche au plus simple
const MIMES: [&str; 4] = [GNOME, URI_LIST, "text/plain;charset=utf-8", "text/plain"];

/// Fichiers mis dans le presse-papiers par cette fenêtre
pub struct Clip {
    pub source: CopyPasteSource,
    pub paths: Vec<PathBuf>,
    pub cut: bool,
}

/// Collage fini : dossier, éléments créés, message pour la barre d'état
pub type Pasted = (PathBuf, Vec<PathBuf>, String);

impl Clip {
    /// Données demandées par l'application qui colle
    pub fn send(&self, mime: &str, mut fd: WritePipe) {
        let uris: String = self.paths.iter().map(|p| info::uri_list(p)).collect();
        let data = match mime {
            GNOME => {
                let verb = if self.cut { "cut" } else { "copy" };
                format!("{verb}\n{}", uris.trim_end().replace("\r\n", "\n"))
            }
            URI_LIST => uris,
            _ => self
                .paths
                .iter()
                .map(|p| p.to_string_lossy())
                .collect::<Vec<_>>()
                .join("\n"),
        };
        let _ = fd.write_all(data.as_bytes());
    }
}

impl Files {
    /// Ctrl+C / Ctrl+X : la sélection dans le presse-papiers
    pub fn clip_selection(&mut self, cut: bool) {
        if self.in_trash() {
            return;
        }
        let paths = self.selected_paths();
        let (Some(manager), Some(device)) = (self.data_manager.as_ref(), self.data_device.as_ref())
        else {
            return;
        };
        if paths.is_empty() {
            return;
        }
        let source = manager.create_copy_paste_source(&self.qh, MIMES);
        source.set_selection(device, self.serial);
        let n = paths.len();
        self.browser.message = Some(match (n, cut) {
            (1, false) => "Copié — Ctrl+V pour coller".into(),
            (1, true) => "Coupé — Ctrl+V pour déplacer ici ou ailleurs".into(),
            (n, false) => format!("{n} éléments copiés — Ctrl+V pour coller"),
            (n, true) => format!("{n} éléments coupés — Ctrl+V pour les déplacer"),
        });
        self.clip = Some(Clip { source, paths, cut });
        self.draw();
    }

    /// Le presse-papiers contient-il des fichiers ?
    pub fn can_paste(&self) -> bool {
        self.clip.is_some()
            || self
                .data_device
                .as_ref()
                .and_then(|d| d.data().selection_offer())
                .is_some_and(|o| {
                    o.with_mime_types(|m| m.iter().any(|t| t == URI_LIST || t == GNOME))
                })
    }

    /// Ctrl+V : colle dans le dossier affiché. La copie se fait dans un fil ;
    /// la liste se met à jour d'elle-même (surveillance du dossier).
    pub fn paste(&mut self) {
        if self.in_trash() {
            return;
        }
        let dir = self.browser.cwd.clone();
        let tx = self.pastes.clone();
        // Nos propres fichiers : sans passer par le protocole
        if let Some(clip) = self.clip.as_ref() {
            let (paths, cut) = (clip.paths.clone(), clip.cut);
            if cut {
                // Un déplacement ne se fait qu'une fois
                self.clip = None;
                if let Some(device) = self.data_device.as_ref() {
                    device.unset_selection(self.serial);
                }
            }
            self.browser.message = Some("Collage…".into());
            self.draw();
            std::thread::spawn(move || {
                let _ = tx.send(paste_into(&paths, cut, &dir));
            });
            return;
        }
        let Some(offer) = self
            .data_device
            .as_ref()
            .and_then(|d| d.data().selection_offer())
        else {
            return;
        };
        let mime = offer.with_mime_types(|m| {
            [GNOME, URI_LIST]
                .into_iter()
                .find(|want| m.iter().any(|t| t == want))
        });
        let Some(mime) = mime else {
            self.browser.message = Some("Le presse-papiers ne contient pas de fichiers".into());
            return self.draw();
        };
        let Ok(pipe) = offer.receive(mime.to_string()) else {
            return;
        };
        let fd = OwnedFd::from(pipe);
        self.browser.message = Some("Collage…".into());
        self.draw();
        std::thread::spawn(move || {
            let mut text = String::new();
            let _ = std::fs::File::from(fd).read_to_string(&mut text);
            let (paths, cut) = parse(&text, mime == GNOME);
            let _ = tx.send(paste_into(&paths, cut, &dir));
        });
    }

    /// Collage fini : les éléments collés deviennent la sélection
    pub fn pasted(&mut self, (dir, created, message): Pasted) {
        if self.browser.cwd == dir {
            self.browser.reload();
            let picked: std::collections::HashSet<PathBuf> = created.into_iter().collect();
            if let Some(first) = self
                .browser
                .entries
                .iter()
                .position(|e| picked.contains(&e.path))
            {
                self.browser.pick_set(picked, first);
            }
        }
        self.browser.message = Some(message);
        self.navigated();
    }
}

/// Liste d'URI (ou format de GNOME : « copy » / « cut » en première ligne)
fn parse(text: &str, gnome: bool) -> (Vec<PathBuf>, bool) {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let cut = gnome && lines.first() == Some(&"cut");
    let paths = lines
        .into_iter()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.strip_prefix("file://"))
        // file://hôte/chemin : le chemin commence au premier « / »
        .filter_map(|l| l.find('/').map(|i| &l[i..]))
        .map(|p| PathBuf::from(unescape(p)))
        .collect();
    (paths, cut)
}

/// Décode les %XX d'une URI (octets bruts : un nom peut ne pas être UTF-8)
fn unescape(s: &str) -> std::ffi::OsString {
    use std::os::unix::ffi::OsStringExt;
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(hex) = s.get(i + 1..i + 3)
            && let Ok(b) = u8::from_str_radix(hex, 16)
        {
            out.push(b);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    std::ffi::OsString::from_vec(out)
}

/// « photo.png » déjà là : « photo copie.png », puis « photo copie 2.png »…
fn copy_name(dir: &Path, name: &str, is_dir: bool) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 && !is_dir => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    std::iter::once(format!("{stem} copie{ext}"))
        .chain((2..).map(|n| format!("{stem} copie {n}{ext}")))
        .map(|n| dir.join(n))
        .find(|p| !p.exists())
        .unwrap_or(first)
}

/// Copie (cp -a) ou déplace chaque élément dans `dir`
fn paste_into(paths: &[PathBuf], cut: bool, dir: &Path) -> Pasted {
    let mut created = Vec::new();
    let mut errors = Vec::new();
    for src in paths {
        let Some(name) = src.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            continue;
        };
        if src.is_dir() && dir.starts_with(src) {
            errors.push(format!("« {name} » ne peut aller dans lui-même"));
            continue;
        }
        if cut {
            if src.parent() == Some(dir) {
                continue;
            }
            match dnd::move_path(src, dir, false) {
                Ok(()) => created.push(dir.join(&name)),
                Err(e) => errors.push(e),
            }
            continue;
        }
        let target = copy_name(dir, &name, src.is_dir());
        let out = std::process::Command::new("cp")
            .args(["-a", "-T", "--"])
            .arg(src)
            .arg(&target)
            .output();
        match out {
            Ok(o) if o.status.success() => created.push(target),
            Ok(o) => errors.push(format!(
                "Copie de « {name} » impossible : {}",
                String::from_utf8_lossy(&o.stderr).trim()
            )),
            Err(e) => errors.push(format!("Copie impossible : {e}")),
        }
    }
    let n = created.len();
    let verb = if cut { "déplacé" } else { "collé" };
    let message = match (n, errors.first()) {
        (0, Some(e)) => e.clone(),
        (_, Some(e)) => format!("{n} élément(s) {verb}(s) ; {e}"),
        (0, None) => "Rien à coller ici".into(),
        (1, None) => format!(
            "« {} » {verb}",
            created[0]
                .file_name()
                .map(|n| n.to_string_lossy())
                .unwrap_or_default()
        ),
        (n, None) => format!("{n} éléments {verb}s"),
    };
    (dir.to_path_buf(), created, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_uri_list_and_gnome() {
        let (p, cut) = parse(
            "file:///home/a/b%20c.txt\r\n# x\r\nfile:///tmp/d\r\n",
            false,
        );
        assert_eq!(
            p,
            [PathBuf::from("/home/a/b c.txt"), PathBuf::from("/tmp/d")]
        );
        assert!(!cut);
        let (p, cut) = parse("cut\nfile://hote/tmp/%C3%A9t%C3%A9", true);
        assert_eq!(p, [PathBuf::from("/tmp/été")]);
        assert!(cut);
    }
}
