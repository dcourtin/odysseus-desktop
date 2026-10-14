//! Archives : reconnaissance (zip, tar compressé ou non, fichier compressé
//! seul) et décompression à côté de l'archive, comme l'Utilitaire d'archive de
//! macOS : un seul élément à la racine est sorti tel quel, sinon tout va dans
//! un dossier au nom de l'archive. Jamais d'écrasement : « nom (2) » au besoin.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    Zip,
    Tar,
    /// Fichier compressé seul : programme de décompression
    Single(&'static str),
}

/// Type d'archive et nom sans extension(s)
pub fn kind(path: &Path) -> Option<(Kind, String)> {
    let name = path.file_name()?.to_string_lossy().into_owned();
    let lower = name.to_lowercase();
    let strip = |n: usize| name[..name.len() - n].to_string();
    for ext in [".tar.gz", ".tar.xz", ".tar.bz2", ".tar.zst"] {
        if lower.ends_with(ext) {
            return Some((Kind::Tar, strip(ext.len())));
        }
    }
    for ext in [".tgz", ".txz", ".tbz2", ".tar"] {
        if lower.ends_with(ext) {
            return Some((Kind::Tar, strip(ext.len())));
        }
    }
    if lower.ends_with(".zip") {
        return Some((Kind::Zip, strip(4)));
    }
    for (ext, prog) in [
        (".gz", "gzip"),
        (".xz", "xz"),
        (".bz2", "bzip2"),
        (".zst", "zstd"),
    ] {
        if lower.ends_with(ext) {
            return Some((Kind::Single(prog), strip(ext.len())));
        }
    }
    None
}

/// Nom libre dans `dir` : « nom », sinon « nom (2) », « nom (3) »…
fn free_name(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.to_string(), String::new()),
    };
    (2..)
        .map(|i| dir.join(format!("{stem} ({i}){ext}")))
        .find(|p| !p.exists())
        .unwrap()
}

fn run(cmd: &mut Command) -> Result<(), String> {
    let out = cmd.output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(err.lines().last().unwrap_or("échec").trim().to_string())
    }
}

/// Décompresse ; renvoie ce qui a été créé (dossier ou fichier)
pub fn extract(path: &Path) -> Result<PathBuf, String> {
    let (kind, base) = kind(path).ok_or("Ce n'est pas une archive reconnue")?;
    let dir = path.parent().ok_or("dossier introuvable")?;
    if let Kind::Single(prog) = kind {
        // Fichier compressé seul : sortie à côté, l'original est gardé
        let dest = free_name(dir, &base);
        let out = Command::new(prog)
            .args(["-d", "-c"])
            .arg(path)
            .output()
            .map_err(|e| format!("{prog} : {e}"))?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
        }
        std::fs::write(&dest, out.stdout).map_err(|e| e.to_string())?;
        return Ok(dest);
    }
    // Dossier de travail caché, à côté de l'archive (même disque : renommages instantanés)
    let work = free_name(dir, &format!(".{base}.décompression"));
    std::fs::create_dir(&work).map_err(|e| e.to_string())?;
    let result = match kind {
        Kind::Zip => run(Command::new("unzip")
            .args(["-q", "-o"])
            .arg(path)
            .arg("-d")
            .arg(&work)),
        _ => run(Command::new("tar")
            .arg("-xf")
            .arg(path)
            .arg("-C")
            .arg(&work)),
    };
    if let Err(e) = result {
        let _ = std::fs::remove_dir_all(&work);
        return Err(e);
    }
    let entries: Vec<PathBuf> = std::fs::read_dir(&work)
        .map_err(|e| e.to_string())?
        .flatten()
        .map(|e| e.path())
        .collect();
    let dest = if let [single] = entries.as_slice() {
        // Un seul élément à la racine : il prend sa place à côté de l'archive
        let name = single
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or(base);
        let dest = free_name(dir, &name);
        std::fs::rename(single, &dest).map_err(|e| e.to_string())?;
        let _ = std::fs::remove_dir(&work);
        dest
    } else {
        let dest = free_name(dir, &base);
        std::fs::rename(&work, &dest).map_err(|e| e.to_string())?;
        dest
    };
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds() {
        assert_eq!(
            kind(Path::new("/a/linux-7.1.tar.xz")),
            Some((Kind::Tar, "linux-7.1".into()))
        );
        assert_eq!(
            kind(Path::new("/a/Photos.ZIP")),
            Some((Kind::Zip, "Photos".into()))
        );
        assert_eq!(
            kind(Path::new("/a/journal.log.gz")),
            Some((Kind::Single("gzip"), "journal.log".into()))
        );
        assert_eq!(kind(Path::new("/a/notes.txt")), None);
    }

    #[test]
    fn round_trip() {
        let dir = std::env::temp_dir().join(format!("odys-archive-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src/projet")).unwrap();
        std::fs::write(dir.join("src/projet/a.txt"), "bonjour").unwrap();
        std::fs::write(dir.join("src/b.txt"), "salut").unwrap();
        // Deux éléments à la racine : un dossier au nom de l'archive
        run(Command::new("tar")
            .args(["-czf"])
            .arg(dir.join("deux.tar.gz"))
            .arg("-C")
            .arg(dir.join("src"))
            .args(["projet", "b.txt"]))
        .unwrap();
        let out = extract(&dir.join("deux.tar.gz")).unwrap();
        assert_eq!(out, dir.join("deux"));
        assert!(out.join("projet/a.txt").exists());
        // Un seul dossier à la racine : il sort tel quel ; une seconde fois, « (2) »
        run(Command::new("tar")
            .args(["-czf"])
            .arg(dir.join("un.tgz"))
            .arg("-C")
            .arg(dir.join("src"))
            .arg("projet"))
        .unwrap();
        assert_eq!(extract(&dir.join("un.tgz")).unwrap(), dir.join("projet"));
        assert_eq!(
            extract(&dir.join("un.tgz")).unwrap(),
            dir.join("projet (2)")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
