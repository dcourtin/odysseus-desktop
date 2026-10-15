//! Mises à jour : rapport écrit par odysseus-veille
//! (~/.local/state/odysseus/mises-a-jour.json), lu par la barre (icône) et par
//! Réglages > Mises à jour. Les versions écartées par l'utilisateur sont dans
//! ~/.config/odysseus/mises-a-jour-ignorees, une par ligne : « nom version ».
//!
//! ```json
//! {
//!   "verifie": 1792060000,
//!   "elements": [
//!     { "nom": "openssl", "genre": "systeme", "installe": "4.0.1", "disponible": "4.0.3",
//!       "niveau": "securite", "note": "",
//!       "avis": [{ "id": "sa-13.1-046", "date": "2026-10-02", "gravite": "High" }] }
//!   ],
//!   "erreurs": ["…"]
//! }
//! ```

use std::{collections::HashSet, path::PathBuf};

use serde_json::{Value, json};

/// D'où vient la mise à jour, et donc comment la faire
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Le bureau lui-même : nouveaux commits sur le dépôt
    Odysseus,
    /// Logiciel construit depuis ses sources (/sources)
    System,
    /// Outil installé à part (chaîne Rust…)
    Tool,
}

/// Importance, pour trier et pour l'icône de la barre
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// Un avis de sécurité est paru depuis l'installation
    Security,
    /// Version corrective de la même branche (ou branche terminée)
    Fix,
    /// Nouvelle version
    Release,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Advisory {
    pub id: String,
    pub date: String,
    pub severity: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Update {
    pub name: String,
    pub kind: Kind,
    pub installed: String,
    pub available: String,
    pub level: Level,
    pub advisories: Vec<Advisory>,
    /// Précision : commits à venir, branche du noyau terminée…
    pub note: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    /// Dernière vérification (secondes depuis 1970)
    pub checked: i64,
    pub updates: Vec<Update>,
    pub errors: Vec<String>,
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into()))
}

pub fn report_path() -> PathBuf {
    home().join(".local/state/odysseus/mises-a-jour.json")
}

pub fn ignored_path() -> PathBuf {
    home().join(".config/odysseus/mises-a-jour-ignorees")
}

impl Kind {
    fn key(self) -> &'static str {
        match self {
            Kind::Odysseus => "odysseus",
            Kind::System => "systeme",
            Kind::Tool => "outil",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "odysseus" => Kind::Odysseus,
            "outil" => Kind::Tool,
            _ => Kind::System,
        }
    }
}

impl Level {
    fn key(self) -> &'static str {
        match self {
            Level::Security => "securite",
            Level::Fix => "correctif",
            Level::Release => "version",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "securite" => Level::Security,
            "correctif" => Level::Fix,
            _ => Level::Release,
        }
    }
}

impl Report {
    pub fn to_json(&self) -> Value {
        json!({
            "verifie": self.checked,
            "elements": self.updates.iter().map(|u| json!({
                "nom": u.name,
                "genre": u.kind.key(),
                "installe": u.installed,
                "disponible": u.available,
                "niveau": u.level.key(),
                "note": u.note,
                "avis": u.advisories.iter().map(|a| json!({
                    "id": a.id, "date": a.date, "gravite": a.severity,
                })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "erreurs": self.errors,
        })
    }

    pub fn from_json(v: &Value) -> Self {
        let s = |v: &Value, k: &str| v[k].as_str().unwrap_or_default().to_string();
        Self {
            checked: v["verifie"].as_i64().unwrap_or(0),
            updates: v["elements"]
                .as_array()
                .map(|list| {
                    list.iter()
                        .map(|u| Update {
                            name: s(u, "nom"),
                            kind: Kind::parse(u["genre"].as_str().unwrap_or("")),
                            installed: s(u, "installe"),
                            available: s(u, "disponible"),
                            level: Level::parse(u["niveau"].as_str().unwrap_or("")),
                            note: s(u, "note"),
                            advisories: u["avis"]
                                .as_array()
                                .map(|a| {
                                    a.iter()
                                        .map(|a| Advisory {
                                            id: s(a, "id"),
                                            date: s(a, "date"),
                                            severity: s(a, "gravite"),
                                        })
                                        .collect()
                                })
                                .unwrap_or_default(),
                        })
                        .collect()
                })
                .unwrap_or_default(),
            errors: v["erreurs"]
                .as_array()
                .map(|e| {
                    e.iter()
                        .filter_map(|e| e.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
        }
    }
}

/// Dernier rapport ; aucun si la veille n'a jamais tourné
pub fn load() -> Option<Report> {
    let text = std::fs::read_to_string(report_path()).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    Some(Report::from_json(&v))
}

/// Enregistre (écriture puis renommage : un lecteur ne voit jamais un fichier à moitié écrit)
pub fn save(report: &Report) -> std::io::Result<()> {
    let path = report_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    let text = serde_json::to_string_pretty(&report.to_json()).map_err(std::io::Error::other)?;
    std::fs::write(&tmp, text + "\n")?;
    std::fs::rename(tmp, path)
}

/// Versions écartées : (nom, version)
pub fn ignored() -> HashSet<(String, String)> {
    std::fs::read_to_string(ignored_path())
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let (n, v) = l.trim().split_once(' ')?;
            Some((n.to_string(), v.trim().to_string()))
        })
        .collect()
}

/// Écarte cette version ; la suivante sera de nouveau proposée
pub fn ignore(name: &str, version: &str) -> std::io::Result<()> {
    let path = ignored_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut text = std::fs::read_to_string(&path).unwrap_or_default();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&format!("{name} {version}\n"));
    std::fs::write(path, text)
}

/// Mises à jour encore à décider (sans celles écartées), les plus importantes d'abord
pub fn pending(report: &Report) -> Vec<Update> {
    let skip = ignored();
    let mut list: Vec<Update> = report
        .updates
        .iter()
        .filter(|u| !skip.contains(&(u.name.clone(), u.available.clone())))
        .cloned()
        .collect();
    list.sort_by(|a, b| {
        (a.kind != Kind::Odysseus, a.level, &a.name).cmp(&(
            b.kind != Kind::Odysseus,
            b.level,
            &b.name,
        ))
    });
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_round_trip() {
        let r = Report {
            checked: 42,
            updates: vec![Update {
                name: "openssl".into(),
                kind: Kind::System,
                installed: "4.0.1".into(),
                available: "4.0.3".into(),
                level: Level::Security,
                advisories: vec![Advisory {
                    id: "sa-13.1-046".into(),
                    date: "2026-10-02".into(),
                    severity: "High".into(),
                }],
                note: String::new(),
            }],
            errors: vec!["réseau".into()],
        };
        assert_eq!(Report::from_json(&r.to_json()), r);
    }
}
