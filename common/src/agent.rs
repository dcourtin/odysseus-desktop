//! Configuration de l'agent du bureau : ~/.config/odysseus/agent.json, un
//! fichier que l'utilisateur peut aussi modifier à la main. Réglages > Agent
//! n'en est qu'une interface : les clés inconnues sont gardées à
//! l'enregistrement, une valeur absente ou invalide reprend sa valeur par
//! défaut. Lu par Réglages, odysseus-mcp (autorisations, à chaque appel) et le
//! panneau Assistant.
//!
//! ```json
//! {
//!   "agent": "claude",
//!   "claude": { "commande": "~/.local/bin/claude" },
//!   "ollama": { "adresse": "http://127.0.0.1:11434", "modele": "qwen3:14b", "contexte": 16384 },
//!   "commande": { "commande": "mon-agent --question" },
//!   "modeles_conseilles": [
//!     { "modele": "qwen3:14b", "nom": "Qwen3 14B", "taille": "9,3 Go", "atouts": "…" }
//!   ],
//!   "autorisations": {
//!     "lire_systeme": true, "lire_documentation": true, "lire_fichiers": false,
//!     "notes": false, "voir_bureau": true, "voir_ecran": false, "agir": false,
//!     "presse_papiers": false, "executer": false
//!   }
//! }
//! ```

use std::path::PathBuf;

use serde_json::{Value, json};

/// Autorisations des agents sur le bureau (odysseus-mcp), par catégorie
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AgentPerms {
    /// État du système : charge, températures, mémoire, disques, réseau, écran, réglages, journaux
    pub read: bool,
    /// Documentation : man pages, doc du noyau, info, notes de construction, bibliothèque
    pub docs: bool,
    /// Fichiers de l'utilisateur et du système (jamais les secrets : clés, mots de passe)
    pub files: bool,
    /// Notes de l'appli Notes : les lire, les écrire, les mettre à la corbeille
    pub notes: bool,
    /// Fenêtres, bureaux, historique des notifications
    pub desktop: bool,
    /// Captures de l'écran ou d'une fenêtre (envoyées à l'agent)
    pub screen: bool,
    /// Actions : notifier, ouvrir, changer de bureau ou de fenêtre, régler le thème…
    pub act: bool,
    /// Lire et écrire le presse-papiers
    pub clipboard: bool,
    /// Lancer des commandes, chacune confirmée par l'utilisateur
    pub run: bool,
}

impl Default for AgentPerms {
    fn default() -> Self {
        Self {
            read: true,
            docs: true,
            files: false,
            notes: false,
            desktop: true,
            screen: false,
            act: false,
            clipboard: false,
            run: false,
        }
    }
}

/// Un modèle du catalogue proposé dans Réglages (Ollama)
#[derive(Clone, Debug, PartialEq)]
pub struct ModelCard {
    pub model: String,
    pub name: String,
    pub size: String,
    pub notes: String,
}

#[derive(Clone, Debug)]
pub struct AgentConfig {
    /// « claude », « ollama » ou « commande »
    pub agent: String,
    /// Commande de Claude Code (sinon ~/.local/bin/claude)
    pub claude_command: Option<String>,
    pub ollama_url: String,
    pub ollama_model: String,
    /// Fenêtre de contexte demandée à Ollama, en jetons (documentation longue)
    pub ollama_ctx: u32,
    /// Commande personnalisée : reçoit la question en argument
    pub custom_command: String,
    pub models: Vec<ModelCard>,
    pub perms: AgentPerms,
    /// Fichier tel que lu : ses clés inconnues sont gardées à l'enregistrement
    raw: Value,
}

/// Catalogue par défaut : modèles conseillés pour une carte de 16 Go
fn default_models() -> Vec<ModelCard> {
    [
        (
            "qwen3:14b",
            "Qwen3 14B",
            "9,3 Go",
            "Recommandé : appels d'outils fiables, bon français",
        ),
        (
            "gpt-oss:20b",
            "gpt-oss 20B",
            "14 Go",
            "Raisonnement et outils ; remplit presque la carte graphique",
        ),
        (
            "gemma3:12b",
            "Gemma 3 12B",
            "8,1 Go",
            "Voit les images (captures d'écran) ; outils moins constants",
        ),
        (
            "qwen3:8b",
            "Qwen3 8B",
            "5,2 Go",
            "Plus léger et plus rapide, y compris sans carte graphique",
        ),
    ]
    .into_iter()
    .map(|(m, n, s, a)| ModelCard {
        model: m.into(),
        name: n.into(),
        size: s.into(),
        notes: a.into(),
    })
    .collect()
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            agent: "claude".into(),
            claude_command: None,
            ollama_url: "http://127.0.0.1:11434".into(),
            ollama_model: "qwen3:14b".into(),
            ollama_ctx: 16384,
            custom_command: String::new(),
            models: default_models(),
            perms: AgentPerms::default(),
            raw: json!({}),
        }
    }
}

pub fn path() -> Option<PathBuf> {
    crate::theme::config_path().map(|p| p.with_file_name("agent.json"))
}

fn home() -> String {
    std::env::var("HOME").unwrap_or_default()
}

impl AgentConfig {
    /// Lit le fichier ; absent, on part des valeurs par défaut (et des
    /// anciennes clés agent_* du fichier reglages, s'il y en a)
    pub fn load() -> Self {
        let raw = path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| serde_json::from_str::<Value>(&t).ok());
        match raw {
            Some(raw) => Self::from_json(raw),
            None => Self::migrate(),
        }
    }

    pub fn from_json(raw: Value) -> Self {
        let mut c = Self::default();
        let s = |v: &Value| v.as_str().map(String::from);
        if let Some(a) = s(&raw["agent"]) {
            c.agent = a;
        }
        c.claude_command = s(&raw["claude"]["commande"]).filter(|v| !v.is_empty());
        if let Some(v) = s(&raw["ollama"]["adresse"]).filter(|v| !v.is_empty()) {
            c.ollama_url = v;
        }
        if let Some(v) = s(&raw["ollama"]["modele"]).filter(|v| !v.is_empty()) {
            c.ollama_model = v;
        }
        if let Some(n) = raw["ollama"]["contexte"]
            .as_u64()
            .filter(|n| (2048..=262_144).contains(n))
        {
            c.ollama_ctx = n as u32;
        }
        c.custom_command = s(&raw["commande"]["commande"]).unwrap_or_default();
        if let Some(list) = raw["modeles_conseilles"].as_array() {
            let models: Vec<ModelCard> = list
                .iter()
                .filter_map(|m| {
                    Some(ModelCard {
                        model: s(&m["modele"])?,
                        name: s(&m["nom"]).unwrap_or_else(|| s(&m["modele"]).unwrap_or_default()),
                        size: s(&m["taille"]).unwrap_or_default(),
                        notes: s(&m["atouts"]).unwrap_or_default(),
                    })
                })
                .collect();
            if !models.is_empty() {
                c.models = models;
            }
        }
        let p = &raw["autorisations"];
        let flag = |k: &str, d: bool| p[k].as_bool().unwrap_or(d);
        let d = AgentPerms::default();
        c.perms = AgentPerms {
            read: flag("lire_systeme", d.read),
            docs: flag("lire_documentation", d.docs),
            files: flag("lire_fichiers", d.files),
            notes: flag("notes", d.notes),
            desktop: flag("voir_bureau", d.desktop),
            screen: flag("voir_ecran", d.screen),
            act: flag("agir", d.act),
            clipboard: flag("presse_papiers", d.clipboard),
            run: flag("executer", d.run),
        };
        c.raw = raw;
        c
    }

    /// Premier lancement : reprend les clés agent_* de l'ancien fichier reglages
    fn migrate() -> Self {
        let mut c = Self::default();
        let text = crate::settings::path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .unwrap_or_default();
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let (k, v) = (k.trim(), v.trim());
            let yes = v == "oui";
            match k {
                "agent" if !v.is_empty() => c.agent = v.into(),
                "agent_commande" => c.custom_command = v.into(),
                "agent_modele" if !v.is_empty() => c.ollama_model = v.into(),
                "agent_lecture" => c.perms.read = yes,
                "agent_bureau" => c.perms.desktop = yes,
                "agent_ecran" => c.perms.screen = yes,
                "agent_actions" => c.perms.act = yes,
                "agent_presse_papiers" => c.perms.clipboard = yes,
                _ => {}
            }
        }
        c
    }

    /// Commande de Claude Code, « ~ » remplacé
    pub fn claude(&self) -> PathBuf {
        let cmd = self
            .claude_command
            .clone()
            .unwrap_or_else(|| "~/.local/bin/claude".into());
        PathBuf::from(cmd.replacen('~', &home(), 1))
    }

    pub fn to_json(&self) -> Value {
        let mut raw = if self.raw.is_object() {
            self.raw.clone()
        } else {
            json!({})
        };
        // Les sous-objets gardent aussi leurs clés inconnues
        let obj = |raw: &mut Value, key: &str| {
            if !raw[key].is_object() {
                raw[key] = json!({});
            }
        };
        raw["agent"] = json!(self.agent);
        obj(&mut raw, "claude");
        match &self.claude_command {
            Some(c) => raw["claude"]["commande"] = json!(c),
            None => {
                if let Some(o) = raw["claude"].as_object_mut() {
                    o.remove("commande");
                }
            }
        }
        obj(&mut raw, "ollama");
        raw["ollama"]["adresse"] = json!(self.ollama_url);
        raw["ollama"]["modele"] = json!(self.ollama_model);
        raw["ollama"]["contexte"] = json!(self.ollama_ctx);
        obj(&mut raw, "commande");
        raw["commande"]["commande"] = json!(self.custom_command);
        raw["modeles_conseilles"] = Value::Array(
            self.models
                .iter()
                .map(|m| json!({"modele": m.model, "nom": m.name, "taille": m.size, "atouts": m.notes}))
                .collect(),
        );
        let p = self.perms;
        raw["autorisations"] = json!({
            "lire_systeme": p.read,
            "lire_documentation": p.docs,
            "lire_fichiers": p.files,
            "notes": p.notes,
            "voir_bureau": p.desktop,
            "voir_ecran": p.screen,
            "agir": p.act,
            "presse_papiers": p.clipboard,
            "executer": p.run,
        });
        raw
    }

    /// Enregistre (écriture puis renommage), en JSON lisible
    pub fn save(&self) -> std::io::Result<()> {
        let path = path().ok_or_else(|| std::io::Error::other("HOME inconnu"))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(&self.to_json()).map_err(std::io::Error::other)?;
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, text + "\n")?;
        std::fs::rename(tmp, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_unknown_keys_and_defaults() {
        let raw = json!({
            "agent": "ollama",
            "ma_note": "à garder",
            "ollama": {"modele": "mistral-small3.2:24b", "options": {"num_ctx": 16384}},
            "modeles_conseilles": [{"modele": "mistral-small3.2:24b", "nom": "Mistral Small"}],
            "autorisations": {"voir_ecran": true}
        });
        let c = AgentConfig::from_json(raw);
        assert_eq!(c.agent, "ollama");
        assert_eq!(c.ollama_model, "mistral-small3.2:24b");
        assert_eq!(c.ollama_url, "http://127.0.0.1:11434");
        assert_eq!(c.models.len(), 1);
        assert!(c.perms.screen && c.perms.read && c.perms.docs && !c.perms.act && !c.perms.run);
        let out = c.to_json();
        assert_eq!(out["ma_note"], "à garder");
        assert_eq!(out["ollama"]["options"]["num_ctx"], 16384);
        assert_eq!(AgentConfig::from_json(out).perms, c.perms);
    }

    #[test]
    fn invalid_values_fall_back() {
        let c = AgentConfig::from_json(
            json!({"autorisations": {"agir": "oui"}, "modeles_conseilles": []}),
        );
        assert!(!c.perms.act);
        assert_eq!(c.models.len(), 4);
    }
}
