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
//!     "notes": false, "memoire": true, "voir_bureau": true, "voir_ecran": false,
//!     "agir": false, "presse_papiers": false, "executer": false,
//!     "executer_sans_accord": false
//!   },
//!   "suggestions": ["Qu'est-ce que j'ai manqué ?", "…"],
//!   "suggestions_contexte": {
//!     "selection": ["Explique ça", "Traduis en anglais"],
//!     "dossier": ["Que contient ce dossier ?"],
//!     "notification": ["Que faire de cette notification ?"],
//!     "presse_papiers": ["Explique ce que j'ai copié"]
//!   }
//! }
//! ```
//!
//! Les suggestions sont les questions proposées par l'Assistant quand la
//! conversation est vide ; celles du contexte passent devant quand la pastille
//! correspondante est là (texte sélectionné, dossier de Fichiers…).

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
    /// Mémoire : lire les faits retenus et les conversations passées, proposer
    /// d'en retenir ou d'en oublier (chaque fois avec l'accord de l'utilisateur)
    pub memory: bool,
    /// Fenêtres, bureaux, historique des notifications
    pub desktop: bool,
    /// Captures de l'écran ou d'une fenêtre (envoyées à l'agent)
    pub screen: bool,
    /// Actions : notifier, ouvrir, changer de bureau ou de fenêtre, régler le thème…
    pub act: bool,
    /// Lire et écrire le presse-papiers
    pub clipboard: bool,
    /// Lancer des commandes, chacune confirmée par l'utilisateur…
    pub run: bool,
    /// … sauf si l'utilisateur a choisi de ne pas être consulté (mode
    /// automatique) : chaque commande reste consignée dans le journal
    pub run_auto: bool,
}

impl Default for AgentPerms {
    fn default() -> Self {
        Self {
            read: true,
            docs: true,
            files: false,
            notes: false,
            memory: true,
            desktop: true,
            screen: false,
            act: false,
            clipboard: false,
            run: false,
            run_auto: false,
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
    /// Questions proposées quand la conversation est vide (quatre affichées)
    pub suggestions: Vec<String>,
    /// Questions proposées selon le contexte présent : clé (selection, dossier,
    /// notification, presse_papiers) et ses questions
    pub context_suggestions: Vec<(String, Vec<String>)>,
    /// Fichier tel que lu : ses clés inconnues sont gardées à l'enregistrement
    raw: Value,
}

/// Les questions de départ : récurrentes, et propres à ce bureau
fn default_suggestions() -> Vec<String> {
    [
        "Qu'est-ce que j'ai manqué ?",
        "Y a-t-il des mises à jour à faire ?",
        "Range mes Téléchargements",
        "Qu'est-ce que tu sais de moi ?",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// Clés des contextes qui ont leurs propres suggestions
pub const CONTEXT_KEYS: [&str; 4] = ["selection", "dossier", "notification", "presse_papiers"];

/// Les questions qui vont avec ce qu'on a sous les yeux
fn default_context_suggestions() -> Vec<(String, Vec<String>)> {
    [
        (
            "selection",
            vec!["Explique ça", "Traduis en anglais", "Reformule plus court"],
        ),
        (
            "dossier",
            vec!["Que contient ce dossier ?", "Range ce dossier par type"],
        ),
        ("notification", vec!["Que faire de cette notification ?"]),
        ("presse_papiers", vec!["Explique ce que j'ai copié"]),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.into_iter().map(String::from).collect()))
    .collect()
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
            suggestions: default_suggestions(),
            context_suggestions: default_context_suggestions(),
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
        let strings = |v: &Value| -> Option<Vec<String>> {
            v.as_array().map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str())
                    .map(str::trim)
                    .filter(|x| !x.is_empty())
                    .map(String::from)
                    .collect()
            })
        };
        if let Some(list) = strings(&raw["suggestions"]).filter(|l| !l.is_empty()) {
            c.suggestions = list;
        }
        // Une clé présente remplace sa liste (vide : rien pour ce contexte) ; une
        // clé absente garde la liste par défaut
        for (key, list) in &mut c.context_suggestions {
            if let Some(given) = strings(&raw["suggestions_contexte"][key.as_str()]) {
                *list = given;
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
            memory: flag("memoire", d.memory),
            desktop: flag("voir_bureau", d.desktop),
            screen: flag("voir_ecran", d.screen),
            act: flag("agir", d.act),
            clipboard: flag("presse_papiers", d.clipboard),
            run: flag("executer", d.run),
            run_auto: flag("executer_sans_accord", d.run_auto),
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
        raw["suggestions"] = json!(self.suggestions);
        raw["suggestions_contexte"] = Value::Object(
            self.context_suggestions
                .iter()
                .map(|(k, v)| (k.clone(), json!(v)))
                .collect(),
        );
        let p = self.perms;
        raw["autorisations"] = json!({
            "lire_systeme": p.read,
            "lire_documentation": p.docs,
            "lire_fichiers": p.files,
            "notes": p.notes,
            "memoire": p.memory,
            "voir_bureau": p.desktop,
            "voir_ecran": p.screen,
            "agir": p.act,
            "presse_papiers": p.clipboard,
            "executer": p.run,
            "executer_sans_accord": p.run_auto,
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
        assert!(c.perms.memory && !c.perms.run_auto);
        let auto = AgentConfig::from_json(json!({"autorisations": {"executer": true, "executer_sans_accord": true}}));
        assert!(auto.perms.run && auto.perms.run_auto);
        assert_eq!(auto.to_json()["autorisations"]["executer_sans_accord"], true);
        assert_eq!(c.suggestions.len(), 4);
        assert_eq!(c.context_suggestions.len(), 4);
        let custom = AgentConfig::from_json(json!({
            "suggestions": ["Une seule", "  "],
            "suggestions_contexte": {"selection": [], "dossier": ["Trie-moi ça"]}
        }));
        assert_eq!(custom.suggestions, vec!["Une seule"]);
        let get = |k: &str| {
            custom
                .context_suggestions
                .iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.clone())
                .unwrap()
        };
        assert!(get("selection").is_empty());
        assert_eq!(get("dossier"), vec!["Trie-moi ça"]);
        assert_eq!(get("notification").len(), 1);
        let back = AgentConfig::from_json(custom.to_json());
        assert_eq!(back.suggestions, custom.suggestions);
        assert!(
            get("selection").is_empty() && back.context_suggestions == custom.context_suggestions
        );
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
