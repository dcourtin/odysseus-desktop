//! Annuler (Ctrl+Z) : chaque opération sur les fichiers est notée, la dernière
//! se défait — mise à la corbeille, renommage, déplacement, création (copie,
//! collage, duplication, nouveau dossier, archive).

use std::path::{Path, PathBuf};

use crate::Files;

/// Au-delà, les plus anciennes opérations sont oubliées
const DEPTH: usize = 30;

pub enum Op {
    /// Mis à la corbeille : emplacements d'origine
    Trashed(Vec<PathBuf>),
    Renamed {
        from: PathBuf,
        to: PathBuf,
    },
    /// Déplacés : (avant, après)
    Moved(Vec<(PathBuf, PathBuf)>),
    /// Créés (copie, dossier, archive) : l'annulation les met à la corbeille
    Created(Vec<PathBuf>),
}

impl Files {
    pub fn record(&mut self, op: Op) {
        let empty = match &op {
            Op::Trashed(v) | Op::Created(v) => v.is_empty(),
            Op::Moved(v) => v.is_empty(),
            Op::Renamed { .. } => false,
        };
        if empty {
            return;
        }
        self.undo.push(op);
        if self.undo.len() > DEPTH {
            self.undo.remove(0);
        }
    }

    /// Ctrl+Z : défait la dernière opération
    pub fn undo(&mut self) {
        let Some(op) = self.undo.pop() else {
            self.browser.message = Some("Rien à annuler".into());
            return self.draw();
        };
        let (message, back) = match op {
            Op::Trashed(originals) => {
                let (n, error) = restore_trashed(&originals);
                (
                    summary(n, error, &originals, "remis en place", "remis en place"),
                    originals,
                )
            }
            Op::Renamed { from, to } => {
                let old = name(&from);
                match move_back(&to, &from) {
                    Ok(()) => (format!("Nom « {old} » rétabli"), vec![from]),
                    Err(e) => (e, Vec::new()),
                }
            }
            Op::Moved(pairs) => {
                let mut done = Vec::new();
                let mut error = None;
                for (from, to) in pairs.iter().rev() {
                    match move_back(to, from) {
                        Ok(()) => done.push(from.clone()),
                        Err(e) => error = Some(e),
                    }
                }
                let n = done.len();
                (
                    summary(n, error, &done, "remis à sa place", "remis à leur place"),
                    done,
                )
            }
            Op::Created(paths) => {
                let mut n = 0;
                let mut error = None;
                for p in &paths {
                    match common::trash::trash(p) {
                        Ok(()) => n += 1,
                        Err(e) => error = Some(e),
                    }
                }
                (
                    summary(
                        n,
                        error,
                        &paths,
                        "retiré (corbeille)",
                        "retirés (corbeille)",
                    ),
                    Vec::new(),
                )
            }
        };
        self.browser.reload();
        // Ce qui revient dans le dossier affiché est sélectionné
        let picked: std::collections::HashSet<PathBuf> = back.into_iter().collect();
        if let Some(first) = self
            .browser
            .entries
            .iter()
            .position(|e| picked.contains(&e.path))
        {
            self.browser.pick_set(picked, first);
        }
        self.browser.message = Some(message);
        self.navigated();
    }
}

fn name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// « « a » remis en place », « 3 éléments remis en place », erreur sinon
fn summary(n: usize, error: Option<String>, paths: &[PathBuf], one: &str, many: &str) -> String {
    match (n, error) {
        (0, Some(e)) => e,
        (_, Some(e)) => format!("{n} élément(s) {many} ; {e}"),
        (0, None) => "Rien à annuler".into(),
        (1, None) => format!(
            "« {} » {one}",
            paths.first().map(|p| name(p)).unwrap_or_default()
        ),
        (n, None) => format!("{n} éléments {many}"),
    }
}

/// Remet `now` à `before` sans rien écraser (mv : aussi d'un disque à l'autre)
fn move_back(now: &Path, before: &Path) -> Result<(), String> {
    if before.exists() {
        return Err(format!(
            "« {} » existe déjà : rien n'est écrasé",
            name(before)
        ));
    }
    if !now.exists() {
        return Err(format!("« {} » n'est plus là", name(now)));
    }
    if std::fs::rename(now, before).is_ok() {
        return Ok(());
    }
    let out = std::process::Command::new("mv")
        .args(["-n", "-T", "--"])
        .arg(now)
        .arg(before)
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Ressort de la corbeille les éléments mis à ces emplacements
fn restore_trashed(originals: &[PathBuf]) -> (usize, Option<String>) {
    let items = common::trash::list();
    let mut restored = 0;
    let mut error = None;
    for original in originals {
        // Le plus récent, si le même nom y est passé plusieurs fois
        let item = items
            .iter()
            .filter(|i| &i.original == original)
            .max_by_key(|i| std::fs::metadata(&i.info).and_then(|m| m.modified()).ok());
        match item {
            Some(item) => match common::trash::restore(item) {
                Ok(()) => restored += 1,
                Err(e) => error = Some(e),
            },
            None => error = Some("Introuvable dans la corbeille".into()),
        }
    }
    (restored, error)
}
