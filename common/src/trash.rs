//! Corbeille au format freedesktop (compatible avec les autres bureaux) :
//! ~/.local/share/Trash/{files,info} pour les fichiers du disque personnel,
//! <point de montage>/.Trash-<uid> pour ceux d'un autre disque (clé USB…).
//! Un fichier mis à la corbeille est déplacé, jamais copié : c'est instantané.

use std::{
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

/// Un élément de la corbeille
pub struct Item {
    /// Chemin dans la corbeille
    pub path: PathBuf,
    /// Emplacement d'origine
    pub original: PathBuf,
    pub info: PathBuf,
}

fn home_trash() -> Option<PathBuf> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
    Some(data.join("Trash"))
}

fn uid() -> u32 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("Uid:"))
                .and_then(|v| v.split_whitespace().next()?.parse().ok())
        })
        .unwrap_or(0)
}

/// Point de montage contenant `path` (le plus long préfixe de /proc/self/mounts).
fn mount_point(path: &Path) -> Option<PathBuf> {
    let mounts = std::fs::read_to_string("/proc/self/mounts").ok()?;
    mounts
        .lines()
        .filter_map(|l| l.split_whitespace().nth(1))
        .map(|m| PathBuf::from(m.replace("\\040", " ")))
        .filter(|m| path.starts_with(m))
        .max_by_key(|m| m.as_os_str().len())
}

/// Corbeille à utiliser pour `path`, et racine des chemins Path= relatifs
/// (None : chemins absolus, corbeille personnelle).
fn trash_for(path: &Path) -> Result<(PathBuf, Option<PathBuf>), String> {
    let home = home_trash().ok_or("dossier personnel introuvable")?;
    let home_dev = home
        .ancestors()
        .find_map(|a| a.metadata().ok())
        .map(|m| m.dev());
    let file_dev = path.symlink_metadata().map(|m| m.dev()).ok();
    if file_dev.is_some() && file_dev == home_dev {
        return Ok((home, None));
    }
    let top = mount_point(path).ok_or("point de montage introuvable")?;
    Ok((top.join(format!(".Trash-{}", uid())), Some(top)))
}

/// Encodage des chemins dans .trashinfo (pourcent, comme une URL)
fn encode(path: &Path) -> String {
    let mut out = String::new();
    for b in path.to_string_lossy().bytes() {
        if b.is_ascii_alphanumeric() || b"/-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Met `path` (fichier ou dossier) à la corbeille.
pub fn trash(path: &Path) -> Result<(), String> {
    let path = std::path::absolute(path).map_err(|e| e.to_string())?;
    let (trash, top) = trash_for(&path)?;
    let files = trash.join("files");
    let info = trash.join("info");
    for dir in [&files, &info] {
        std::fs::create_dir_all(dir).map_err(|e| format!("corbeille : {e}"))?;
    }
    let name = path
        .file_name()
        .ok_or("chemin sans nom")?
        .to_string_lossy()
        .into_owned();
    // Nom libre dans la corbeille : « photo.jpg », « photo 2.jpg »…
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.clone(), String::new()),
    };
    let mut candidate = name.clone();
    let mut n = 2;
    let info_file = loop {
        let info_file = info.join(format!("{candidate}.trashinfo"));
        // Création exclusive : réserve le nom
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&info_file)
        {
            Ok(_) if !files.join(&candidate).exists() => break info_file,
            Ok(_) => {
                let _ = std::fs::remove_file(&info_file);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(format!("corbeille : {e}")),
        }
        candidate = format!("{stem} {n}{ext}");
        n += 1;
    };
    let recorded = match &top {
        Some(top) => path.strip_prefix(top).unwrap_or(&path).to_path_buf(),
        None => path.clone(),
    };
    let date = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S");
    let content = format!(
        "[Trash Info]\nPath={}\nDeletionDate={date}\n",
        encode(&recorded)
    );
    std::fs::write(&info_file, content).map_err(|e| format!("corbeille : {e}"))?;
    if let Err(e) = std::fs::rename(&path, files.join(&candidate)) {
        let _ = std::fs::remove_file(&info_file);
        return Err(format!("mise à la corbeille impossible : {e}"));
    }
    Ok(())
}

/// Contenu de la corbeille personnelle, du plus récent au plus ancien.
pub fn list() -> Vec<Item> {
    let Some(trash) = home_trash() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(trash.join("info")) else {
        return Vec::new();
    };
    let mut items: Vec<(std::time::SystemTime, Item)> = entries
        .flatten()
        .filter_map(|e| {
            let info = e.path();
            let name = info.file_name()?.to_string_lossy();
            let name = name.strip_suffix(".trashinfo")?.to_string();
            let text = std::fs::read_to_string(&info).ok()?;
            let original = text
                .lines()
                .find_map(|l| l.strip_prefix("Path="))
                .map(|p| PathBuf::from(decode(p)))?;
            let when = e.metadata().and_then(|m| m.modified()).ok()?;
            let path = trash.join("files").join(&name);
            path.symlink_metadata().ok()?;
            Some((
                when,
                Item {
                    path,
                    original,
                    info,
                },
            ))
        })
        .collect();
    items.sort_by_key(|item| std::cmp::Reverse(item.0));
    items.into_iter().map(|(_, i)| i).collect()
}

/// Remet un élément à son emplacement d'origine (s'il est libre).
pub fn restore(item: &Item) -> Result<(), String> {
    if item.original.symlink_metadata().is_ok() {
        return Err(format!("« {} » existe déjà", item.original.display()));
    }
    if let Some(parent) = item.original.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::rename(&item.path, &item.original).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&item.info);
    Ok(())
}

/// Vide la corbeille personnelle (suppression définitive).
pub fn empty() -> Result<usize, String> {
    let items = list();
    let n = items.len();
    for item in items {
        let meta = item.path.symlink_metadata().map_err(|e| e.to_string())?;
        if meta.is_dir() {
            std::fs::remove_dir_all(&item.path).map_err(|e| e.to_string())?;
        } else {
            std::fs::remove_file(&item.path).map_err(|e| e.to_string())?;
        }
        let _ = std::fs::remove_file(&item.info);
    }
    Ok(n)
}

/// Dossier des fichiers de la corbeille personnelle (pour l'afficher dans Fichiers)
pub fn files_dir() -> Option<PathBuf> {
    home_trash().map(|t| t.join("files"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodage() {
        let p = Path::new("/home/d/Été 2026/photo #1.jpg");
        assert_eq!(decode(&encode(p)), p.to_string_lossy());
    }

    #[test]
    fn aller_retour() {
        let dir = std::env::temp_dir().join(format!("odysseus-trash-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // SAFETY : test isolé, aucune autre lecture de l'environnement en parallèle
        unsafe { std::env::set_var("XDG_DATA_HOME", &dir) };
        let f = dir.join("essai.txt");
        std::fs::write(&f, "x").unwrap();
        trash(&f).unwrap();
        assert!(!f.exists());
        let items = list();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].original, f);
        restore(&items[0]).unwrap();
        assert!(f.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
