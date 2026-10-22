//! La mémoire de l'Assistant : des faits durables sur l'utilisateur et sa
//! machine (« préfère les réponses courtes », « le disque Archives est un
//! NAS »), une ligne chacun, dans ~/.config/odysseus/memoire.md — un fichier
//! que l'utilisateur lit et modifie à la main, comme le Dock ou le thème.
//!
//! Ils sont donnés au modèle en tête de chaque conversation (`prompt`).
//! Un agent n'y ajoute ni n'y retire rien sans l'accord de l'utilisateur
//! (odysseus-mcp le demande dans une notification), et n'y met jamais de
//! secret (`check`). La date et l'origine de chaque fait sont gardées à
//! part, dans ~/.local/state/odysseus/memoire.json : le fichier reste un
//! simple texte.
//!
//! Ce qui est à l'écran ou dans le dossier ouvert n'est pas un fait : les
//! outils le donnent sur le moment, rien n'en est gardé ici.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::text::fold;

/// Au-delà, retenir refuse : la mémoire tient en une page
pub const MAX_FACTS: usize = 40;
/// Un fait tient en une phrase ou deux
pub const MAX_FACT_CHARS: usize = 200;
/// Ce qui est donné au modèle (les petits contextes d'Ollama)
pub const MAX_PROMPT_BYTES: usize = 2048;

const HEADER: &str = "# Mémoire de l'Assistant

Une ligne par fait, commençant par « - ». L'Assistant lit ce fichier au début
de chaque conversation ; il ne propose d'y ajouter ou d'en retirer que ce que
vous acceptez (notification), et vous pouvez l'éditer ici ou dans Réglages >
Agent. Jamais de mot de passe ni de clé.

";

#[derive(Clone, Debug, PartialEq)]
pub struct Fact {
    pub text: String,
    /// Quand il a été retenu, secondes depuis 1970 ; absent : écrit à la main
    pub date: Option<i64>,
    /// D'où il vient : « conversation « … » » ; absent : écrit à la main
    pub origin: Option<String>,
}

/// Le fichier des faits et celui de leur état
pub struct Memory {
    file: PathBuf,
    state: PathBuf,
}

/// Le fichier des faits (~/.config/odysseus/memoire.md)
pub fn path() -> Option<PathBuf> {
    crate::theme::config_path().map(|p| p.with_file_name("memoire.md"))
}

fn state_path() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
        .join(".local/state/odysseus/memoire.json")
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Espaces repliés, bords ôtés : le texte tel qu'il est gardé
pub fn normalize(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Ce qui ressemble à un secret n'entre pas dans la mémoire : mots de passe,
/// jetons, clés, ou une longue suite de signes sans espace comme en ont les
/// clés et les jetons
pub fn looks_secret(text: &str) -> bool {
    let low = fold(text);
    const WORDS: [&str; 14] = [
        "mot de passe",
        "mdp",
        "password",
        "passwd",
        "passphrase",
        "jeton",
        "token",
        "api key",
        "apikey",
        "api_key",
        "cle privee",
        "private key",
        "secret",
        "bearer ",
    ];
    if WORDS.iter().any(|w| low.contains(w)) {
        return true;
    }
    // sk-…, ghp_…, AKIA… : des préfixes de jetons connus
    if ["sk-", "ghp_", "gho_", "xoxb-", "xoxp-", "akia"]
        .iter()
        .any(|p| low.split_whitespace().any(|w| w.starts_with(p)))
    {
        return true;
    }
    // Un mot de 24 signes ou plus mêlant lettres et chiffres : clé, jeton, empreinte
    text.split_whitespace().any(|w| {
        let w = w.trim_matches(|c: char| !c.is_alphanumeric());
        w.chars().count() >= 24
            && w.chars().any(|c| c.is_ascii_digit())
            && w.chars().any(|c| c.is_ascii_alphabetic())
            && w.chars()
                .all(|c| c.is_ascii_alphanumeric() || "+/=_-".contains(c))
    })
}

/// Un fait recevable : une ligne, courte, sans secret ; renvoie le texte normalisé
pub fn check(text: &str) -> Result<String, String> {
    let t = normalize(text);
    if t.is_empty() {
        return Err("Fait vide".into());
    }
    if t.chars().count() > MAX_FACT_CHARS {
        return Err(format!(
            "Trop long ({} caractères, {MAX_FACT_CHARS} au plus) : la mémoire garde des faits, \
             pas des documents. Résumez en une phrase.",
            t.chars().count()
        ));
    }
    if looks_secret(&t) {
        return Err(
            "Refusé : cela ressemble à un secret (mot de passe, jeton, clé). La mémoire n'en \
             garde jamais."
                .into(),
        );
    }
    Ok(t)
}

/// Une ligne du fichier qui porte un fait : « - … » ou « * … »
fn fact_line(line: &str) -> Option<&str> {
    let t = line.trim_start();
    t.strip_prefix("- ")
        .or_else(|| t.strip_prefix("* "))
        .map(str::trim)
        .filter(|f| !f.is_empty())
}

impl Memory {
    pub fn open() -> Option<Self> {
        Some(Self {
            file: path()?,
            state: state_path(),
        })
    }

    #[cfg(test)]
    fn with(file: &Path, state: &Path) -> Self {
        Self {
            file: file.to_path_buf(),
            state: state.to_path_buf(),
        }
    }

    pub fn file(&self) -> &Path {
        &self.file
    }

    /// Le fichier existe, avec son en-tête : de quoi l'ouvrir dans un éditeur
    pub fn ensure(&self) -> Result<(), String> {
        if self.file.exists() {
            return Ok(());
        }
        if let Some(d) = self.file.parent() {
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        std::fs::write(&self.file, HEADER).map_err(|e| e.to_string())
    }

    fn read_state(&self) -> Map<String, Value> {
        std::fs::read_to_string(&self.state)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default()
    }

    fn write_state(&self, state: &Map<String, Value>) -> Result<(), String> {
        if let Some(d) = self.state.parent() {
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        let tmp = self.state.with_extension("tmp");
        std::fs::write(
            &tmp,
            serde_json::to_string_pretty(&Value::Object(state.clone())).unwrap_or_default(),
        )
        .map_err(|e| e.to_string())?;
        std::fs::rename(tmp, &self.state).map_err(|e| e.to_string())
    }

    /// Les faits, dans l'ordre du fichier, avec date et origine quand on les connaît
    pub fn facts(&self) -> Vec<Fact> {
        let text = std::fs::read_to_string(&self.file).unwrap_or_default();
        let state = self.read_state();
        text.lines()
            .filter_map(fact_line)
            .map(|f| {
                let meta = state.get(f);
                Fact {
                    text: f.to_string(),
                    date: meta.and_then(|m| m["date"].as_i64()),
                    origin: meta.and_then(|m| m["origine"].as_str()).map(String::from),
                }
            })
            .collect()
    }

    /// Le fait déjà présent qui dit la même chose, s'il y en a un
    pub fn find(&self, text: &str) -> Option<String> {
        let key = fold(&normalize(text));
        self.facts()
            .into_iter()
            .map(|f| f.text)
            .find(|f| fold(f) == key)
    }

    /// Ajoute un fait à la fin du fichier (créé avec son en-tête s'il n'existe
    /// pas) ; le reste du fichier n'est pas touché
    pub fn add(&self, text: &str, origin: Option<&str>) -> Result<String, String> {
        let t = check(text)?;
        if let Some(same) = self.find(&t) {
            return Err(format!("Déjà retenu : « {same} »"));
        }
        let mut content = std::fs::read_to_string(&self.file).unwrap_or_default();
        if content.lines().filter_map(fact_line).count() >= MAX_FACTS {
            return Err(format!(
                "La mémoire est pleine ({MAX_FACTS} faits) : proposez d'en oublier un d'abord."
            ));
        }
        if content.trim().is_empty() {
            content = HEADER.to_string();
        } else if !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str(&format!("- {t}\n"));
        if let Some(d) = self.file.parent() {
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        let tmp = self.file.with_extension("tmp");
        std::fs::write(&tmp, &content).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &self.file).map_err(|e| e.to_string())?;
        let mut state = self.read_state();
        let mut meta = json!({"date": now()});
        if let Some(o) = origin.map(normalize).filter(|o| !o.is_empty()) {
            meta["origine"] = json!(o);
        }
        state.insert(t.clone(), meta);
        let _ = self.write_state(&state);
        Ok(t)
    }

    /// Retire la ligne de ce fait ; vrai s'il y était
    pub fn remove(&self, text: &str) -> Result<bool, String> {
        let Some(found) = self.find(text) else {
            return Ok(false);
        };
        let content = std::fs::read_to_string(&self.file).map_err(|e| e.to_string())?;
        let kept: Vec<&str> = content
            .lines()
            .filter(|l| fact_line(l) != Some(found.as_str()))
            .collect();
        let tmp = self.file.with_extension("tmp");
        std::fs::write(&tmp, kept.join("\n") + "\n").map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &self.file).map_err(|e| e.to_string())?;
        let mut state = self.read_state();
        if state.remove(&found).is_some() {
            let _ = self.write_state(&state);
        }
        Ok(true)
    }

    /// Ce qui est donné au modèle en tête de conversation ; rien s'il n'y a
    /// aucun fait. Tronqué à la ligne si la page est trop longue.
    pub fn prompt(&self) -> Option<String> {
        let facts = self.facts();
        if facts.is_empty() {
            return None;
        }
        let mut out = String::from(
            "Ce que l'utilisateur a accepté que tu retiennes (sa mémoire, Réglages > Agent) :\n",
        );
        let mut cut = 0;
        for f in &facts {
            let line = format!("- {}\n", f.text);
            if out.len() + line.len() > MAX_PROMPT_BYTES {
                cut += 1;
                continue;
            }
            out.push_str(&line);
        }
        if cut > 0 {
            out.push_str(&format!(
                "({cut} autres faits non montrés : la mémoire est trop longue, l'outil memoire les donne.)\n"
            ));
        }
        Some(out)
    }
}

/// Les faits du fichier de l'utilisateur ; rien si HOME est inconnu
pub fn facts() -> Vec<Fact> {
    Memory::open().map(|m| m.facts()).unwrap_or_default()
}

/// Ce que le modèle reçoit en tête de conversation, s'il y a des faits
pub fn prompt() -> Option<String> {
    Memory::open().and_then(|m| m.prompt())
}

// --- La machine ------------------------------------------------------------------

/// Le gestionnaire de paquets de la machine, d'après les commandes présentes
fn package_manager() -> Option<&'static str> {
    const MANAGERS: [(&str, &str); 8] = [
        ("apt-get", "apt (Debian, Ubuntu…)"),
        ("pacman", "pacman (Arch…)"),
        ("dnf", "dnf (Fedora…)"),
        ("zypper", "zypper (openSUSE)"),
        ("apk", "apk (Alpine)"),
        ("xbps-install", "xbps (Void)"),
        ("emerge", "portage (Gentoo)"),
        ("nix-env", "nix"),
    ];
    let path = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin:/usr/sbin:/sbin".into());
    MANAGERS.iter().find_map(|(bin, label)| {
        path.split(':')
            .any(|d| Path::new(d).join(bin).is_file())
            .then_some(*label)
    })
}

/// Un paragraphe sur la machine pour le prompt système : distribution, noyau,
/// gestionnaire de paquets (ou sources compilées à la main), dossiers des
/// outils de l'utilisateur. Rien n'est codé en dur : le même bureau tourne
/// sur un Linux From Scratch et sur une Debian.
pub fn machine() -> String {
    let release = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    let pretty = release
        .lines()
        .find_map(|l| l.strip_prefix("PRETTY_NAME="))
        .map(|v| v.trim_matches('"').to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "Linux".into());
    let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|k| k.trim().to_string())
        .unwrap_or_default();
    let home = std::env::var("HOME").unwrap_or_default();
    let mut out = format!("{pretty}, noyau {kernel}, systemd, bureau Wayland en Rust (odysseus). ");
    match package_manager() {
        Some(pm) => out.push_str(&format!("Gestionnaire de paquets : {pm}. ")),
        None if Path::new("/sources/blfs-desktop").is_dir() => out.push_str(
            "Il n'y a aucun gestionnaire de paquets (ni apt, ni pacman, ni dnf) : les logiciels \
             sont compilés depuis leurs sources (scripts et journal dans /sources/blfs-desktop). ",
        ),
        None => out.push_str("Aucun gestionnaire de paquets repéré. "),
    }
    let user_dirs: Vec<&str> = [
        (".local/bin", "~/.local/bin"),
        (".cargo/bin", "~/.cargo/bin (Rust par rustup)"),
        ("Applications", "~/Applications (AppImage)"),
    ]
    .iter()
    .filter(|(d, _)| Path::new(&home).join(d).is_dir())
    .map(|(_, label)| *label)
    .collect();
    if !user_dirs.is_empty() {
        out.push_str(&format!(
            "Les logiciels de l'utilisateur sont dans {}.",
            user_dirs.join(", ")
        ));
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> (PathBuf, Memory) {
        let dir =
            std::env::temp_dir().join(format!("odysseus-memoire-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let m = Memory::with(&dir.join("memoire.md"), &dir.join("etat/memoire.json"));
        (dir, m)
    }

    #[test]
    fn secrets_are_refused() {
        for s in [
            "Le mot de passe du NAS est hunter2",
            "Token GitHub : ghp_abcdef",
            "Clé API sk-proj-abc",
            "Empreinte 3f9a8c1d2e4b5a6f7c8d9e0a1b2c3d4e",
        ] {
            assert!(looks_secret(s), "{s}");
            assert!(check(s).is_err(), "{s}");
        }
        for s in [
            "Préfère les réponses courtes",
            "Le disque « Archives » est un NAS Synology",
            "Travaille en PHP, Python et JavaScript",
            "Né le 12/03/1980, fête le 12 mars",
        ] {
            assert!(!looks_secret(s), "{s}");
            assert!(check(s).is_ok(), "{s}");
        }
        assert!(check("   ").is_err());
        assert!(check(&"x".repeat(MAX_FACT_CHARS + 1)).is_err());
    }

    #[test]
    fn add_find_remove_keep_hand_written_text() {
        let (dir, m) = temp("vie");
        assert!(m.facts().is_empty());
        assert!(m.prompt().is_none());
        let kept = m
            .add(
                "  Préfère   les réponses courtes ",
                Some("conversation « Bonjour »"),
            )
            .unwrap();
        assert_eq!(kept, "Préfère les réponses courtes");
        // Le même, autrement accentué : déjà là
        assert!(m.add("PREFERE les reponses courtes", None).is_err());
        // L'utilisateur écrit à la main dans le fichier
        let mut text = std::fs::read_to_string(m.file()).unwrap();
        text.push_str("\nUn commentaire à moi.\n- Le NAS s'appelle Archives\n");
        std::fs::write(m.file(), text).unwrap();
        let facts = m.facts();
        assert_eq!(facts.len(), 2);
        assert_eq!(facts[0].origin.as_deref(), Some("conversation « Bonjour »"));
        assert!(facts[0].date.is_some());
        assert_eq!(facts[1].text, "Le NAS s'appelle Archives");
        assert!(facts[1].date.is_none() && facts[1].origin.is_none());
        let p = m.prompt().unwrap();
        assert!(p.contains("- Préfère les réponses courtes\n- Le NAS s'appelle Archives\n"));
        assert!(m.remove("prefere les reponses courtes").unwrap());
        assert!(!m.remove("prefere les reponses courtes").unwrap());
        let text = std::fs::read_to_string(m.file()).unwrap();
        assert!(text.contains("Un commentaire à moi."));
        assert!(text.contains("# Mémoire de l'Assistant"));
        assert_eq!(m.facts().len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn full_and_truncated() {
        let (dir, m) = temp("pleine");
        for i in 0..MAX_FACTS {
            m.add(&format!("Fait numéro {i} {}", "x".repeat(60)), None)
                .unwrap();
        }
        assert!(m.add("Un de trop", None).is_err());
        let p = m.prompt().unwrap();
        assert!(p.len() <= MAX_PROMPT_BYTES + 120);
        assert!(p.contains("autres faits non montrés"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn machine_profile_is_not_empty() {
        let m = machine();
        assert!(m.contains("noyau"));
    }
}
