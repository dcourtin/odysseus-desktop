//! Automatismes : des suites d'actions du bureau, lancées à la main, à heure
//! fixe ou à l'arrivée d'un fichier dans un dossier. Un fichier JSON par
//! automatisme dans ~/.config/odysseus/automatismes, modifiable à la main ou
//! écrit par l'agent :
//!
//! ```json
//! {
//!   "nom": "Soirée calme",
//!   "description": "Thème sombre, son coupé, Ne pas déranger",
//!   "declencheurs": [
//!     { "type": "manuel" },
//!     { "type": "heure", "heure": "22:00", "jours": ["lun", "mar", "mer", "jeu", "ven"] },
//!     { "type": "dossier", "dossier": "~/Téléchargements" }
//!   ],
//!   "etapes": [
//!     { "action": "regler", "theme": "sombre" },
//!     { "action": "executer", "commande": "ls ~/Téléchargements", "garder": "liste" },
//!     { "action": "notifier", "titre": "Fichiers", "texte": "{liste}" },
//!     { "si": { "contient": ["{fichier}", ".pdf"] }, "alors": [ … ], "sinon": [ … ] }
//!   ]
//! }
//! ```
//!
//! Chaque étape est un outil d'odysseus-mcp (mêmes noms, mêmes paramètres),
//! ou une étape de contrôle : attendre, choisir, agent, si. Un automatisme
//! ne s'exécute qu'une fois validé par l'utilisateur (Réglages >
//! Automatismes) : la version validée est gardée dans
//! ~/.local/state/odysseus/automatismes.json, hors de portée de l'agent ;
//! toute modification du fichier demande une nouvelle validation.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

/// Étapes de contrôle, en plus des outils d'odysseus-mcp
pub const CONTROL: [&str; 3] = ["attendre", "choisir", "agent"];

/// Outils d'odysseus-mcp utilisables comme étapes
pub const ACTIONS: [&str; 22] = [
    "etat_systeme",
    "reglages",
    "manuel",
    "documentation",
    "lire_fichier",
    "lister_dossier",
    "fenetres",
    "notifications",
    "notifier",
    "ouvrir",
    "changer_bureau",
    "activer_fenetre",
    "regler",
    "lancer_application",
    "fermer_fenetre",
    "deplacer_fenetre",
    "son",
    "ne_pas_deranger",
    "lire_presse_papiers",
    "ecrire_presse_papiers",
    "executer",
    "journaux",
];

const DAYS: [&str; 7] = ["lun", "mar", "mer", "jeu", "ven", "sam", "dim"];

#[derive(Clone, Debug, PartialEq)]
pub enum Trigger {
    Manual,
    /// Heure et minute, jours de la semaine (0 = lundi ; vide = tous les jours)
    Time {
        hour: u32,
        minute: u32,
        days: Vec<u32>,
    },
    /// Nouveau fichier dans un dossier (variable {fichier})
    Folder(PathBuf),
}

impl Trigger {
    pub fn label(&self) -> String {
        match self {
            Trigger::Manual => "À la demande".into(),
            Trigger::Time { hour, minute, days } => {
                let when = if days.is_empty() || days.len() == 7 {
                    "Tous les jours".to_string()
                } else if days == &[0, 1, 2, 3, 4] {
                    "En semaine".to_string()
                } else if days == &[5, 6] {
                    "Le week-end".to_string()
                } else {
                    days.iter()
                        .map(|d| DAYS[*d as usize % 7])
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                format!("{when} à {hour:02}:{minute:02}")
            }
            Trigger::Folder(dir) => format!("Nouveau fichier dans {}", tilde(dir)),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Automatism {
    /// Nom du fichier sans .json : identifiant
    pub id: String,
    pub name: String,
    pub description: String,
    pub triggers: Vec<Trigger>,
    pub steps: Vec<Value>,
    /// Ce qui doit être validé : nom, déclencheurs, étapes
    pub signature: Value,
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
}

pub fn tilde(p: &Path) -> String {
    let h = home();
    match p.strip_prefix(&h) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => p.display().to_string(),
    }
}

pub fn expand(p: &str) -> PathBuf {
    match p.strip_prefix("~/") {
        Some(rest) => home().join(rest),
        None if p == "~" => home(),
        None => PathBuf::from(p),
    }
}

pub fn dir() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config"));
    base.join("odysseus/automatismes")
}

fn state_path() -> PathBuf {
    home().join(".local/state/odysseus/automatismes.json")
}

/// Identifiant de fichier à partir d'un nom : « Soirée calme » → soiree-calme
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.to_lowercase().chars() {
        let c = match c {
            'à' | 'â' | 'ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'î' | 'ï' => 'i',
            'ô' | 'ö' => 'o',
            'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            c if c.is_ascii_alphanumeric() => c,
            _ => '-',
        };
        if c != '-' || !out.ends_with('-') {
            out.push(c);
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        "automatisme".into()
    } else {
        out
    }
}

fn parse_trigger(v: &Value) -> Result<Trigger, String> {
    match v["type"].as_str().unwrap_or("") {
        "manuel" => Ok(Trigger::Manual),
        "heure" => {
            let t = v["heure"]
                .as_str()
                .ok_or("déclencheur heure : « heure » manquante (HH:MM)")?;
            let (h, m) = t
                .split_once(':')
                .and_then(|(h, m)| {
                    Some((h.trim().parse::<u32>().ok()?, m.trim().parse::<u32>().ok()?))
                })
                .filter(|(h, m)| *h < 24 && *m < 60)
                .ok_or_else(|| format!("heure invalide : {t} (HH:MM attendu)"))?;
            let mut days = Vec::new();
            for d in v["jours"].as_array().into_iter().flatten() {
                let d = d.as_str().unwrap_or("");
                let i = DAYS
                    .iter()
                    .position(|x| d.to_lowercase().starts_with(x))
                    .ok_or_else(|| {
                        format!("jour inconnu : {d} (lun, mar, mer, jeu, ven, sam, dim)")
                    })?;
                days.push(i as u32);
            }
            days.sort_unstable();
            days.dedup();
            Ok(Trigger::Time {
                hour: h,
                minute: m,
                days,
            })
        }
        "dossier" => {
            let d = v["dossier"]
                .as_str()
                .ok_or("déclencheur dossier : « dossier » manquant")?;
            Ok(Trigger::Folder(expand(d)))
        }
        other => Err(format!(
            "type de déclencheur inconnu : « {other} » (manuel, heure, dossier)"
        )),
    }
}

/// Vérifie les étapes (récursivement pour « si ») ; renvoie la première erreur
fn check_steps(steps: &[Value], path: &str) -> Result<(), String> {
    if steps.is_empty() {
        return Err(format!("{path}aucune étape"));
    }
    for (i, step) in steps.iter().enumerate() {
        let here = format!("{path}étape {} : ", i + 1);
        let Some(obj) = step.as_object() else {
            return Err(format!("{here}objet attendu"));
        };
        if let Some(cond) = obj.get("si") {
            let ok = ["contient", "egal", "vide", "non_vide"]
                .iter()
                .any(|k| cond.get(k).is_some());
            if !ok {
                return Err(format!(
                    "{here}condition inconnue (contient, egal, vide, non_vide)"
                ));
            }
            let then = obj
                .get("alors")
                .and_then(Value::as_array)
                .ok_or(format!("{here}« alors » manquant"))?;
            check_steps(then, &format!("{here}alors, "))?;
            if let Some(other) = obj.get("sinon").and_then(Value::as_array)
                && !other.is_empty()
            {
                check_steps(other, &format!("{here}sinon, "))?;
            }
            continue;
        }
        let action = obj
            .get("action")
            .and_then(Value::as_str)
            .ok_or(format!("{here}« action » manquante"))?;
        if !ACTIONS.contains(&action) && !CONTROL.contains(&action) {
            return Err(format!(
                "{here}action inconnue « {action} » (outils : {} ; contrôle : {})",
                ACTIONS.join(", "),
                CONTROL.join(", ")
            ));
        }
        if let Some(g) = obj.get("garder").and_then(Value::as_str)
            && (g.is_empty() || !g.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
        {
            return Err(format!(
                "{here}« garder » : lettres sans accent, chiffres ou _ (« {g} »)"
            ));
        }
        let need = |k: &str| -> Result<(), String> {
            obj.get(k)
                .map(|_| ())
                .ok_or(format!("{here}{action} : « {k} » manquant"))
        };
        match action {
            "executer" => need("commande")?,
            "notifier" => need("titre")?,
            "attendre" => need("secondes")?,
            "choisir" => {
                let n = obj
                    .get("choix")
                    .and_then(Value::as_array)
                    .map(Vec::len)
                    .unwrap_or(0);
                if !(1..=3).contains(&n) {
                    return Err(format!("{here}choisir : de 1 à 3 « choix »"));
                }
            }
            "agent" => need("consigne")?,
            _ => {}
        }
    }
    Ok(())
}

/// Paramètres rangés dans un sous-objet (« arguments », « parametres »),
/// comme l'écrivent souvent les modèles : remis à plat dans l'étape
pub fn normalize(def: &mut Value) {
    fn steps(list: &mut Value) {
        for step in list.as_array_mut().into_iter().flatten() {
            let Some(obj) = step.as_object_mut() else {
                continue;
            };
            for key in ["arguments", "parametres", "paramètres", "params", "args"] {
                if let Some(Value::Object(inner)) = obj.remove(key) {
                    for (k, v) in inner {
                        obj.entry(k).or_insert(v);
                    }
                }
            }
            for branch in ["alors", "sinon"] {
                if let Some(b) = obj.get_mut(branch) {
                    steps(b);
                }
            }
        }
    }
    if let Some(list) = def.get_mut("etapes") {
        steps(list);
    }
}

impl Automatism {
    pub fn from_json(id: &str, v: &Value) -> Result<Self, String> {
        let mut v = v.clone();
        normalize(&mut v);
        let v = &v;
        let name = v["nom"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .ok_or("« nom » manquant")?;
        let triggers = match v["declencheurs"].as_array() {
            Some(list) if !list.is_empty() => list
                .iter()
                .map(parse_trigger)
                .collect::<Result<Vec<_>, _>>()?,
            _ => vec![Trigger::Manual],
        };
        let steps = v["etapes"]
            .as_array()
            .cloned()
            .ok_or("« etapes » manquantes")?;
        check_steps(&steps, "")?;
        Ok(Self {
            id: id.to_string(),
            name: name.trim().to_string(),
            description: v["description"].as_str().unwrap_or("").trim().to_string(),
            triggers,
            steps: steps.clone(),
            signature: json!({"nom": name, "declencheurs": v["declencheurs"], "etapes": steps}),
        })
    }

    pub fn path(&self) -> PathBuf {
        dir().join(format!("{}.json", self.id))
    }

    pub fn manual(&self) -> bool {
        self.triggers.contains(&Trigger::Manual)
    }
}

/// Tous les automatismes lisibles, et les fichiers en erreur (id, erreur)
pub fn load_all() -> (Vec<Automatism>, Vec<(String, String)>) {
    let mut ok = Vec::new();
    let mut bad = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir()) else {
        return (ok, bad);
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.extension().is_none_or(|x| x != "json") {
            continue;
        }
        let id = p
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let parsed = std::fs::read_to_string(&p)
            .map_err(|e| e.to_string())
            .and_then(|t| {
                serde_json::from_str::<Value>(&t).map_err(|e| format!("JSON illisible : {e}"))
            })
            .and_then(|v| Automatism::from_json(&id, &v));
        match parsed {
            Ok(a) => ok.push(a),
            Err(e) => bad.push((id, e)),
        }
    }
    ok.sort_by_key(|a| a.name.to_lowercase());
    bad.sort();
    (ok, bad)
}

/// D'après son identifiant ou son nom
pub fn find(key: &str) -> Option<Automatism> {
    let (all, _) = load_all();
    let k = key.trim().to_lowercase();
    all.iter()
        .find(|a| a.id == key)
        .or_else(|| all.iter().find(|a| a.name.to_lowercase() == k))
        .or_else(|| all.iter().find(|a| a.id == slug(key)))
        .cloned()
}

/// Écrit un automatisme (nouveau ou remplacé) ; renvoie son identifiant
pub fn save(def: &Value) -> Result<Automatism, String> {
    let mut def = def.clone();
    normalize(&mut def);
    let def = &def;
    let name = def["nom"].as_str().ok_or("« nom » manquant")?;
    let id = slug(name);
    let a = Automatism::from_json(&id, def)?;
    std::fs::create_dir_all(dir()).map_err(|e| e.to_string())?;
    let text = serde_json::to_string_pretty(def).map_err(|e| e.to_string())?;
    let tmp = a.path().with_extension("tmp");
    std::fs::write(&tmp, text + "\n").map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, a.path()).map_err(|e| e.to_string())?;
    Ok(a)
}

// --- État : validation, pause, dernière exécution ------------------------------------

fn read_state() -> Map<String, Value> {
    std::fs::read_to_string(state_path())
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

fn write_state(state: &Map<String, Value>) -> Result<(), String> {
    let p = state_path();
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    let tmp = p.with_extension("tmp");
    std::fs::write(
        &tmp,
        serde_json::to_string_pretty(&Value::Object(state.clone())).unwrap_or_default(),
    )
    .map_err(|e| e.to_string())?;
    std::fs::rename(tmp, p).map_err(|e| e.to_string())
}

fn update_state(id: &str, f: impl FnOnce(&mut Map<String, Value>)) -> Result<(), String> {
    let mut state = read_state();
    let entry = state.entry(id.to_string()).or_insert_with(|| json!({}));
    if !entry.is_object() {
        *entry = json!({});
    }
    if let Some(obj) = entry.as_object_mut() {
        f(obj);
    }
    write_state(&state)
}

#[derive(Clone, Debug, Default)]
pub struct Status {
    /// La version actuelle du fichier est celle que l'utilisateur a validée
    pub validated: bool,
    /// Une version précédente avait été validée (le fichier a changé depuis)
    pub changed: bool,
    /// Mis en pause : les déclencheurs automatiques ne le lancent plus
    pub paused: bool,
    /// Dernière exécution : date et résultat
    pub last: Option<(String, String)>,
}

pub fn status(a: &Automatism) -> Status {
    let state = read_state();
    let s = &state.get(&a.id).cloned().unwrap_or(json!({}));
    let validated = s.get("valide").is_some_and(|v| *v == a.signature);
    Status {
        validated,
        changed: !validated && s.get("valide").is_some(),
        paused: s["pause"].as_bool().unwrap_or(false),
        last: s["derniere"].as_str().map(|d| {
            (
                d.to_string(),
                s["resultat"].as_str().unwrap_or("").to_string(),
            )
        }),
    }
}

/// L'utilisateur valide la version actuelle (Réglages > Automatismes)
pub fn validate(a: &Automatism) -> Result<(), String> {
    let sig = a.signature.clone();
    update_state(&a.id, |o| {
        o.insert("valide".into(), sig);
        o.insert("pause".into(), json!(false));
    })
}

pub fn set_paused(a: &Automatism, paused: bool) -> Result<(), String> {
    update_state(&a.id, |o| {
        o.insert("pause".into(), json!(paused));
    })
}

pub fn record_run(id: &str, result: &str) {
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
    let _ = update_state(id, |o| {
        o.insert("derniere".into(), json!(now));
        o.insert("resultat".into(), json!(result));
    });
}

pub fn forget(id: &str) {
    let mut state = read_state();
    if state.remove(id).is_some() {
        let _ = write_state(&state);
    }
}

/// Résumé lisible d'une étape (Réglages, validation)
pub fn describe(step: &Value) -> String {
    if let Some(cond) = step.get("si") {
        let c = if let Some(v) = cond.get("contient").and_then(Value::as_array) {
            format!("Si « {} » contient « {} »", txt(v.first()), txt(v.get(1)))
        } else if let Some(v) = cond.get("egal").and_then(Value::as_array) {
            format!("Si « {} » vaut « {} »", txt(v.first()), txt(v.get(1)))
        } else if let Some(v) = cond.get("vide") {
            format!("Si « {} » est vide", txt(Some(v)))
        } else {
            format!("Si « {} » n'est pas vide", txt(cond.get("non_vide")))
        };
        // Les étapes de chaque branche sont listées dessous
        return format!("{c} :");
    }
    let action = step["action"].as_str().unwrap_or("?");
    let keep = step["garder"]
        .as_str()
        .map(|g| format!(" → {{{g}}}"))
        .unwrap_or_default();
    // Commande : en entier, c'est elle que l'utilisateur valide
    if action == "executer" {
        let reason = step["raison"]
            .as_str()
            .filter(|r| !r.is_empty())
            .map(|r| format!(" — {r}"))
            .unwrap_or_default();
        let dir = step["dossier"]
            .as_str()
            .map(|d| format!(" (dans {d})"))
            .unwrap_or_default();
        return format!(
            "Commande : {}{dir}{reason}{keep}",
            txt(step.get("commande"))
        );
    }
    let args: Vec<String> = step
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(k, _)| *k != "action" && *k != "garder")
        .map(|(k, v)| format!("{k} : {}", txt(Some(v))))
        .collect();
    let label = action_label(action);
    if args.is_empty() {
        format!("{label}{keep}")
    } else {
        format!("{label} — {}{keep}", args.join(", "))
    }
}

/// Nom lisible d'une action
pub fn action_label(action: &str) -> &str {
    match action {
        "notifier" => "Notification",
        "regler" => "Réglage du bureau",
        "son" => "Son",
        "ne_pas_deranger" => "Ne pas déranger",
        "ouvrir" => "Ouvrir",
        "lancer_application" => "Lancer une application",
        "fermer_fenetre" => "Fermer une fenêtre",
        "deplacer_fenetre" => "Déplacer une fenêtre",
        "changer_bureau" => "Changer de bureau",
        "activer_fenetre" => "Activer une fenêtre",
        "ecrire_presse_papiers" => "Copier",
        "lire_presse_papiers" => "Lire le presse-papiers",
        "lire_fichier" => "Lire un fichier",
        "lister_dossier" => "Lister un dossier",
        "etat_systeme" => "Mesurer le système",
        "notifications" => "Lire les notifications",
        "fenetres" => "Lister les fenêtres",
        "documentation" => "Documentation",
        "manuel" => "Man page",
        "reglages" => "Lire les réglages",
        "journaux" => "Lire les journaux",
        "attendre" => "Attendre",
        "choisir" => "Choisir",
        "agent" => "Demander au modèle",
        other => other,
    }
}

fn txt(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

/// Étapes à plat, avec leur profondeur (pour l'affichage des « si »)
pub fn flatten(steps: &[Value], depth: usize, out: &mut Vec<(usize, String)>) {
    for s in steps {
        out.push((depth, describe(s)));
        if s.get("si").is_some() {
            if let Some(a) = s.get("alors").and_then(Value::as_array) {
                flatten(a, depth + 1, out);
            }
            if let Some(b) = s
                .get("sinon")
                .and_then(Value::as_array)
                .filter(|b| !b.is_empty())
            {
                out.push((depth, "Sinon :".into()));
                flatten(b, depth + 1, out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Value {
        json!({
            "nom": "Soirée calme",
            "declencheurs": [{"type": "manuel"}, {"type": "heure", "heure": "22:00", "jours": ["lun", "mar", "mer", "jeu", "ven"]}],
            "etapes": [
                {"action": "regler", "theme": "sombre"},
                {"si": {"contient": ["{fichier}", ".pdf"]}, "alors": [{"action": "notifier", "titre": "PDF"}]}
            ]
        })
    }

    #[test]
    fn parses_and_describes() {
        let a = Automatism::from_json("soiree-calme", &sample()).unwrap();
        assert_eq!(a.triggers.len(), 2);
        assert_eq!(a.triggers[1].label(), "En semaine à 22:00");
        let mut lines = Vec::new();
        flatten(&a.steps, 0, &mut lines);
        assert_eq!(lines.len(), 3);
        assert!(lines[0].1.starts_with("Réglage du bureau — theme : sombre"));
        assert_eq!(lines[2].0, 1);
    }

    #[test]
    fn rejects_bad_steps() {
        let mut v = sample();
        v["etapes"] = json!([{"action": "formater_disque"}]);
        assert!(
            Automatism::from_json("x", &v)
                .unwrap_err()
                .contains("inconnue")
        );
        v["etapes"] = json!([{"action": "executer"}]);
        assert!(
            Automatism::from_json("x", &v)
                .unwrap_err()
                .contains("commande")
        );
        v["etapes"] = json!([]);
        assert!(Automatism::from_json("x", &v).is_err());
    }

    #[test]
    fn nested_arguments_are_flattened() {
        let v = json!({"nom": "x", "etapes": [
            {"action": "executer", "arguments": {"commande": "ls"}},
            {"si": {"vide": "{resultat}"}, "alors": [{"action": "notifier", "parametres": {"titre": "t"}}]}
        ]});
        let a = Automatism::from_json("x", &v).unwrap();
        assert_eq!(a.steps[0]["commande"], "ls");
        assert_eq!(a.steps[1]["alors"][0]["titre"], "t");
    }

    #[test]
    fn slugs() {
        assert_eq!(slug("Soirée calme !"), "soiree-calme");
        assert_eq!(slug("Ranger Téléchargements"), "ranger-telechargements");
    }
}
