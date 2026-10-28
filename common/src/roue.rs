//! Roue : jusqu'à huit raccourcis disposés en cercle autour du pointeur
//! (odysseus-roue), choisis dans Réglages > Roue.
//!
//! Une place de la roue peut tenir :
//! - une application (fichier .desktop) : lancée, ou sa fenêtre ramenée ;
//! - une action d'application (section [Desktop Action …] de son .desktop,
//!   comme « Nouvelle note » de Notes) ;
//! - une entrée proposée par une appli. C'est l'interface offerte aux applis :
//!   un fichier .desktop (Name, Exec, Icon, Comment) déposé dans le dossier
//!   `odysseus/roue` d'un dossier de données XDG (`~/.local/share`,
//!   `/usr/share`…), ou écrit par `odysseus-roue proposer`. Une proposition
//!   n'entre jamais seule dans la roue : l'utilisateur l'y place dans Réglages.
//!
//! - un dossier : il ouvre un second anneau autour de la roue, avec jusqu'à
//!   huit places à lui (applications, actions, propositions ; pas de dossier
//!   dans un dossier) ;
//! - le statut : notifications, mises à jour, panneaux du bureau et icônes
//!   des applications, en anneau. Il a toujours une place dans la roue (on
//!   le déplace, on ne le retire pas) ; jamais dans un dossier.
//!
//! Fichier `~/.config/odysseus/roue` : une ligne par place, dans le sens des
//! aiguilles d'une montre depuis le haut — « app:ID », « action:ID:CLÉ »,
//! « entree:ID », « dossier:NOM », « statut » ou « - » (place vide). Les places d'un
//! dossier suivent sa ligne, en retrait. Sans fichier : les favoris du Dock.

use std::path::PathBuf;

use crate::apps::{self, App};

pub const SLOTS: usize = 8;

/// Ce qu'on a mis à une place
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Slot {
    App(String),
    Action(String, String),
    Entree(String),
    Dossier(Box<Folder>),
    Statut,
}

/// Dossier : un nom et ses places, ouvertes en second anneau
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Folder {
    pub name: String,
    pub places: [Option<Slot>; SLOTS],
}

impl Folder {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.into(),
            places: Default::default(),
        }
    }

    /// Places remplies, dans l'ordre
    pub fn filled(&self) -> impl Iterator<Item = &Slot> {
        self.places.iter().flatten()
    }
}

impl Slot {
    pub fn parse(s: &str) -> Option<Slot> {
        if s.trim() == "statut" {
            return Some(Slot::Statut);
        }
        let (kind, rest) = s.trim().split_once(':')?;
        let rest = rest.trim();
        match kind.trim() {
            "app" if !rest.is_empty() => Some(Slot::App(rest.into())),
            "action" => {
                let (id, key) = rest.rsplit_once(':')?;
                (!id.is_empty() && !key.is_empty()).then(|| Slot::Action(id.into(), key.into()))
            }
            "entree" if !rest.is_empty() => Some(Slot::Entree(rest.into())),
            "dossier" if !rest.is_empty() => Some(Slot::Dossier(Box::new(Folder::new(rest)))),
            _ => None,
        }
    }

    pub fn to_text(&self) -> String {
        match self {
            Slot::App(id) => format!("app:{id}"),
            Slot::Action(id, key) => format!("action:{id}:{key}"),
            Slot::Entree(id) => format!("entree:{id}"),
            Slot::Dossier(f) => format!("dossier:{}", f.name),
            Slot::Statut => "statut".into(),
        }
    }

    pub fn folder(&self) -> Option<&Folder> {
        match self {
            Slot::Dossier(f) => Some(f),
            _ => None,
        }
    }

    pub fn folder_mut(&mut self) -> Option<&mut Folder> {
        match self {
            Slot::Dossier(f) => Some(f),
            _ => None,
        }
    }
}

pub type Wheel = [Option<Slot>; SLOTS];

pub fn config_path() -> Option<PathBuf> {
    crate::theme::config_path().map(|p| p.with_file_name("roue"))
}

pub fn parse(text: &str) -> Wheel {
    let mut wheel: Wheel = Default::default();
    // Place courante de la roue, et place suivante dans son dossier
    let mut top: Option<usize> = None;
    let mut inner = 0;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if raw.starts_with([' ', '\t']) {
            // Place d'un dossier : jamais un dossier
            let folder = top
                .and_then(|t| wheel[t].as_mut())
                .and_then(Slot::folder_mut);
            if let Some(folder) = folder
                && inner < SLOTS
            {
                folder.places[inner] =
                    Slot::parse(line).filter(|s| s.folder().is_none() && *s != Slot::Statut);
                inner += 1;
            }
            continue;
        }
        let next = top.map_or(0, |t| t + 1);
        if next >= SLOTS {
            break;
        }
        wheel[next] = Slot::parse(line);
        top = Some(next);
        inner = 0;
    }
    wheel
}

pub fn to_text(wheel: &Wheel) -> String {
    let mut out = String::from(
        "# Roue d'odysseus (Réglages > Roue) : une place par ligne, depuis le haut,\n\
         # dans le sens des aiguilles d'une montre — app:ID, action:ID:CLÉ, entree:ID,\n\
         # dossier:NOM (ses places suivent, en retrait), statut ou -\n",
    );
    for place in wheel {
        out.push_str(&place.as_ref().map_or("-".into(), Slot::to_text));
        out.push('\n');
        if let Some(folder) = place.as_ref().and_then(Slot::folder) {
            // Jusqu'à la dernière place remplie
            let n = folder
                .places
                .iter()
                .rposition(Option::is_some)
                .map_or(0, |i| i + 1);
            for p in &folder.places[..n] {
                out.push_str("  ");
                out.push_str(&p.as_ref().map_or("-".into(), Slot::to_text));
                out.push('\n');
            }
        }
    }
    out
}

/// Roue enregistrée, sinon les favoris du Dock ; le statut toujours à une place
pub fn load(apps: &[App]) -> Wheel {
    let mut wheel = match config_path().and_then(|p| std::fs::read_to_string(p).ok()) {
        Some(text) => parse(&text),
        None => default(apps),
    };
    with_status(&mut wheel);
    wheel
}

/// Place d'où le statut part, s'il n'en a pas encore : en bas
pub const STATUS_HOME: usize = SLOTS / 2;

/// Le statut a toujours une place : s'il manque, la première libre depuis le
/// bas ; roue pleine, il prend celle du bas, et ce qu'elle tenait va à la
/// fin du premier dossier qui a de la place (sinon cette place se perd)
pub fn with_status(wheel: &mut Wheel) {
    if wheel.contains(&Some(Slot::Statut)) {
        return;
    }
    let order = (0..SLOTS).map(|d| (STATUS_HOME + d) % SLOTS);
    if let Some(i) = order.clone().find(|&i| wheel[i].is_none()) {
        wheel[i] = Some(Slot::Statut);
        return;
    }
    let displaced = wheel[STATUS_HOME].replace(Slot::Statut);
    if let Some(slot) = displaced.filter(|s| s.folder().is_none()) {
        let room = wheel
            .iter_mut()
            .flatten()
            .filter_map(Slot::folder_mut)
            .find_map(|f| f.places.iter_mut().find(|p| p.is_none()));
        if let Some(room) = room {
            *room = Some(slot);
        }
    }
}

/// Favoris du Dock (fichier `dock`), sinon une sélection d'applis présentes
pub fn default(apps: &[App]) -> Wheel {
    const FALLBACK: [&str; 6] = [
        "odysseus-files",
        "firefox",
        "foot",
        "odysseus-notes",
        "odysseus-assistant",
        "odysseus-reglages",
    ];
    let pinned: Vec<String> = crate::theme::config_path()
        .map(|p| p.with_file_name("dock"))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|t| {
            t.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .map(String::from)
                .collect()
        })
        .unwrap_or_else(|| FALLBACK.iter().map(|s| s.to_string()).collect());
    let mut wheel: Wheel = Default::default();
    let present = pinned
        .into_iter()
        .filter(|id| apps.iter().any(|a| &a.id == id));
    // La place du bas reste au statut
    let places = (0..SLOTS).filter(|&i| i != STATUS_HOME);
    for (i, id) in places.zip(present) {
        wheel[i] = Some(Slot::App(id));
    }
    wheel[STATUS_HOME] = Some(Slot::Statut);
    wheel
}

/// Enregistre (écriture puis renommage)
pub fn save(wheel: &Wheel) -> std::io::Result<()> {
    let path = config_path().ok_or_else(|| std::io::Error::other("HOME inconnu"))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, to_text(wheel))?;
    std::fs::rename(tmp, path)
}

/// Ce que lance une place
#[derive(Clone, Debug)]
pub struct Item {
    pub slot: Slot,
    pub name: String,
    /// Appli d'une action, description d'une proposition
    pub detail: String,
    /// Nom d'icône ou chemin
    pub icon: String,
    pub command: String,
    /// Indice de l'appli dans la liste (fenêtres ouvertes) : appli seule
    pub app: Option<usize>,
}

/// Tout ce qu'on peut mettre dans la roue : propositions des applis, puis
/// applications, puis leurs actions
pub fn catalog(apps: &[App]) -> Vec<Item> {
    let status = Item {
        slot: Slot::Statut,
        name: "Statut".into(),
        detail: "Notifications, mises à jour, panneaux et icônes des applis".into(),
        icon: String::new(),
        command: String::new(),
        app: None,
    };
    let mut items: Vec<Item> = std::iter::once(status)
        .chain(proposals().into_iter().map(|(id, p)| Item {
            slot: Slot::Entree(id),
            detail: if p.comment.is_empty() {
                "Proposée par une appli".into()
            } else {
                p.comment.clone()
            },
            command: p.command(),
            name: p.name,
            icon: p.icon,
            app: None,
        }))
        .collect();
    for (i, a) in apps.iter().enumerate().filter(|(_, a)| !a.id.is_empty()) {
        items.push(Item {
            slot: Slot::App(a.id.clone()),
            name: a.name.clone(),
            detail: a.comment.clone(),
            icon: a.icon.clone(),
            command: a.command(),
            app: Some(i),
        });
    }
    for a in apps.iter().filter(|a| !a.id.is_empty()) {
        for act in &a.actions {
            let command = if a.terminal {
                let term = std::env::var("TERMINAL").unwrap_or_else(|_| "foot".into());
                format!("{term} -e {}", act.exec)
            } else {
                act.exec.clone()
            };
            items.push(Item {
                slot: Slot::Action(a.id.clone(), act.key.clone()),
                name: act.name.clone(),
                detail: a.name.clone(),
                icon: if act.icon.is_empty() {
                    a.icon.clone()
                } else {
                    act.icon.clone()
                },
                command,
                app: None,
            });
        }
    }
    items
}

/// Dossiers des propositions, du plus prioritaire au moins
fn proposal_dirs() -> Vec<PathBuf> {
    let home = std::env::var("HOME").unwrap_or_default();
    let data_home =
        std::env::var("XDG_DATA_HOME").unwrap_or_else(|_| format!("{home}/.local/share"));
    let mut dirs = vec![PathBuf::from(data_home).join("odysseus/roue")];
    let data_dirs =
        std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    dirs.extend(
        data_dirs
            .split(':')
            .filter(|d| !d.is_empty())
            .map(|d| PathBuf::from(d).join("odysseus/roue")),
    );
    dirs
}

/// Entrées proposées par les applis : (identifiant, contenu lu comme une appli)
pub fn proposals() -> Vec<(String, App)> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for dir in proposal_dirs() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut files: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "desktop"))
            .collect();
        files.sort();
        for path in files {
            let Some(id) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
                continue;
            };
            if !seen.insert(id.clone()) {
                continue;
            }
            if let Some(app) = std::fs::read_to_string(&path)
                .ok()
                .and_then(|c| apps::parse(&c))
            {
                out.push((id, app));
            }
        }
    }
    out
}

/// Identifiant valable pour une proposition (nom de fichier sûr)
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 100
        && !id.starts_with('.')
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

/// Écrit (ou remplace) une proposition dans `~/.local/share/odysseus/roue`
pub fn propose(
    id: &str,
    name: &str,
    command: &str,
    icon: &str,
    comment: &str,
) -> Result<PathBuf, String> {
    if !valid_id(id) {
        return Err(format!(
            "identifiant invalide « {id} » (lettres, chiffres, . - _)"
        ));
    }
    let one_line = |s: &str| s.replace(['\n', '\r'], " ").trim().to_string();
    let (name, command) = (one_line(name), one_line(command));
    if name.is_empty() || command.is_empty() {
        return Err("il faut un nom et une commande".into());
    }
    let dir = proposal_dirs().remove(0);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut text = format!(
        "[Desktop Entry]\nType=Application\nName={name}\nExec={}\n",
        command.replace('%', "%%")
    );
    if !icon.trim().is_empty() {
        text.push_str(&format!("Icon={}\n", one_line(icon)));
    }
    if !comment.trim().is_empty() {
        text.push_str(&format!("Comment={}\n", one_line(comment)));
    }
    let path = dir.join(format!("{id}.desktop"));
    std::fs::write(&path, text).map_err(|e| e.to_string())?;
    Ok(path)
}

/// Retire une proposition écrite pour l'utilisateur
pub fn withdraw(id: &str) -> Result<(), String> {
    if !valid_id(id) {
        return Err(format!("identifiant invalide « {id} »"));
    }
    let path = proposal_dirs().remove(0).join(format!("{id}.desktop"));
    std::fs::remove_file(&path).map_err(|e| format!("{} : {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let mut w: Wheel = Default::default();
        w[0] = Some(Slot::App("odysseus-files".into()));
        w[2] = Some(Slot::Action("odysseus-notes".into(), "nouvelle".into()));
        w[7] = Some(Slot::Entree("com.exemple.capture".into()));
        let mut art = Folder::new("Artcraft");
        art.places[0] = Some(Slot::App("photocraft".into()));
        art.places[2] = Some(Slot::App("vectorcraft".into()));
        w[4] = Some(Slot::Dossier(Box::new(art)));
        assert_eq!(parse(&to_text(&w)), w);
        // Pas de dossier dans un dossier ; un dossier vide reste un dossier
        let w = parse("dossier:A\n  dossier:B\n  app:x\ndossier:Vide\napp:y\n");
        let a = w[0].as_ref().and_then(Slot::folder).unwrap();
        assert_eq!(a.places[0], None);
        assert_eq!(a.places[1], Some(Slot::App("x".into())));
        assert_eq!(
            w[1].as_ref()
                .and_then(Slot::folder)
                .map(|f| f.filled().count()),
            Some(0)
        );
        assert_eq!(w[2], Some(Slot::App("y".into())));
        assert_eq!(
            Slot::parse("action:org.a.B:new-window"),
            Some(Slot::Action("org.a.B".into(), "new-window".into()))
        );
        assert_eq!(Slot::parse("app:"), None);
        assert!(valid_id("com.exemple-1_x") && !valid_id("../x") && !valid_id(".x"));
    }

    #[test]
    fn status() {
        // Lu et écrit ; jamais dans un dossier
        let w = parse("app:a\nstatut\ndossier:D\n  statut\n  app:b\n");
        assert_eq!(w[1], Some(Slot::Statut));
        let d = w[2].as_ref().and_then(Slot::folder).unwrap();
        assert_eq!(d.places[0], None);
        assert_eq!(parse(&to_text(&w)), w);
        // Absent : la première place libre depuis le bas
        let mut w = parse("app:a\n-\n-\n-\napp:b\n-\napp:c\n");
        with_status(&mut w);
        assert_eq!(w[5], Some(Slot::Statut));
        // Roue pleine : la place du bas, ce qu'elle tenait va dans un dossier
        let mut w: Wheel = std::array::from_fn(|i| Some(Slot::App(format!("app{i}"))));
        w[0] = Some(Slot::Dossier(Box::new(Folder::new("D"))));
        with_status(&mut w);
        assert_eq!(w[STATUS_HOME], Some(Slot::Statut));
        let d = w[0].as_ref().and_then(Slot::folder).unwrap();
        assert_eq!(d.places[0], Some(Slot::App(format!("app{STATUS_HOME}"))));
        // Déjà là : rien ne bouge
        let before = w.clone();
        with_status(&mut w);
        assert_eq!(w, before);
    }

    #[test]
    fn desktop_actions() {
        let app = apps::parse(
            "[Desktop Entry]\nType=Application\nName=Notes\nExec=notes %U\nActions=nouvelle;absente;\n\
             [Desktop Action nouvelle]\nName=New note\nName[fr]=Nouvelle note\nExec=notes --nouvelle\n\
             [Desktop Action cachee]\nName=Cachée\nExec=x\n",
        )
        .unwrap();
        assert_eq!(app.exec, "notes");
        assert_eq!(app.actions.len(), 1);
        assert_eq!(app.actions[0].key, "nouvelle");
        assert_eq!(app.actions[0].exec, "notes --nouvelle");
    }
}
