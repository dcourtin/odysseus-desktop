//! Ce que montre la roue : un arbre de places. Les huit places de la roue, et
//! sous chacune ce que son anneau déploie :
//! - un dossier : ses places ;
//! - une appli ouverte : ses fenêtres, puis « Nouvelle fenêtre » ;
//! - le statut : notifications, mises à jour, panneaux du bureau, puis les
//!   icônes des applis (StatusNotifierItem), elles-mêmes avec leur menu.
//!
//! Le même fonctionnement à chaque niveau : survoler une place qui a des
//! enfants ouvre l'anneau suivant.

use common::{
    apps::{self, App},
    draw::Image,
    roue::{self, Item, Slot},
    sni,
    theme::icon,
    updates,
};

/// Ce que fait le choix d'une place
#[derive(Clone)]
pub enum Act {
    /// Lancer une commande
    Run(String),
    /// Appli : ramener sa fenêtre (la suivante si elle est déjà devant), sinon la lancer
    App {
        command: String,
        windows: Vec<(u64, bool)>,
    },
    /// Une fenêtre au premier plan
    Focus(u64),
    /// Place vide : la remplir dans Réglages > Roue
    Settings,
    /// Programme d'odysseus voisin de la roue, et ses arguments
    Sibling(&'static str, &'static [&'static str]),
    /// Ouvrir l'anneau (dossier, statut)
    Open,
    /// Icône d'une appli : son action principale (et où, à l'écran)
    Tray(String, i32, i32),
    /// Ligne du menu d'une icône d'appli
    TrayMenu(String, i32),
    /// Rien : titre de sous-menu, ligne inactive
    Nothing,
}

/// Ce qu'on dessine à la place
pub enum Look {
    /// Icône par nom ou chemin
    Icon(String),
    /// Image déjà lue (icône d'une appli tierce)
    Image(Option<Image>),
    /// Glyphe de la police d'icônes
    Glyph(char),
    /// Pastille avec les premières icônes des enfants
    Folder,
    /// Le statut : pastille d'accent, glyphes de ses premiers enfants
    Status,
    /// Le nom dans une pastille, sans icône (lignes de menu)
    Label,
    /// Place vide
    Empty,
}

/// Pastille en haut à droite : un nombre, « ! », ou vide (un point)
#[derive(Clone)]
pub struct Badge {
    pub text: String,
    /// Rouge (sécurité) plutôt qu'à la couleur d'accent
    pub urgent: bool,
}

pub struct Node {
    pub name: String,
    pub look: Look,
    pub act: Act,
    pub badge: Option<Badge>,
    /// Appli ouverte : un point sous l'icône
    pub running: bool,
    /// Action ou proposition : un repère dans le coin
    pub marker: bool,
    /// Case à cocher d'un menu, et son état
    pub toggle: Option<bool>,
    pub enabled: bool,
    pub children: Vec<Node>,
    /// Icône d'appli dont le menu se lit à son survol
    pub menu_key: Option<String>,
    /// Icônes chargées : à la taille des places, et des aperçus d'un dossier
    pub icon: Option<Option<Image>>,
    pub mini: Option<Option<Image>>,
}

impl Node {
    fn new(name: impl Into<String>, look: Look, act: Act) -> Self {
        Self {
            name: name.into(),
            look,
            act,
            badge: None,
            running: false,
            marker: false,
            toggle: None,
            enabled: true,
            children: Vec::new(),
            menu_key: None,
            icon: None,
            mini: None,
        }
    }

    fn empty() -> Self {
        Self::new("", Look::Empty, Act::Settings)
    }

    /// Icônes à recharger (changement d'échelle), enfants compris
    pub fn forget_icons(&mut self) {
        self.icon = None;
        self.mini = None;
        for c in &mut self.children {
            c.forget_icons();
        }
    }
}

/// Fenêtres ouvertes, d'après le compositeur : (identifiant, app_id, titre, active)
fn windows() -> Vec<(u64, String, String, bool)> {
    let Ok(json) = common::ipc::request(common::ipc::GET_WINDOWS, "") else {
        return Vec::new();
    };
    let Ok(serde_json::Value::Array(list)) = serde_json::from_str(&json) else {
        return Vec::new();
    };
    list.iter()
        .map(|w| {
            let text = |k: &str| w[k].as_str().unwrap_or_default().to_string();
            (
                w["id"].as_u64().unwrap_or(0),
                text("app_id"),
                text("title"),
                w["focused"].as_bool().unwrap_or(false),
            )
        })
        .collect()
}

/// Les huit places de la roue
pub fn build(apps: &[App]) -> Vec<Node> {
    let catalog = roue::catalog(apps);
    let wins = windows();
    roue::load(apps)
        .iter()
        .map(|slot| match slot {
            Some(Slot::Dossier(f)) => {
                let mut node = Node::new(f.name.clone(), Look::Folder, Act::Open);
                node.children = f
                    .filled()
                    .map(|s| place(Some(s), &catalog, &wins, apps))
                    .collect();
                if node.children.is_empty() {
                    node.children.push(Node::empty());
                }
                node
            }
            Some(Slot::Statut) => status(Vec::new()),
            other => place(other.as_ref(), &catalog, &wins, apps),
        })
        .collect()
}

/// Une place : appli (et ses fenêtres), action, proposition, ou vide
fn place(
    slot: Option<&Slot>,
    catalog: &[Item],
    wins: &[(u64, String, String, bool)],
    apps: &[App],
) -> Node {
    let Some(item) = slot.and_then(|s| catalog.iter().find(|it| &it.slot == s)) else {
        return Node::empty();
    };
    let mine: Vec<&(u64, String, String, bool)> = item
        .app
        .map(|ai| {
            wins.iter()
                .filter(|(_, app_id, ..)| apps::for_window(app_id, apps) == Some(ai))
                .collect()
        })
        .unwrap_or_default();
    let act = match item.app {
        Some(_) => Act::App {
            command: item.command.clone(),
            windows: mine.iter().map(|(id, _, _, f)| (*id, *f)).collect(),
        },
        None => Act::Run(item.command.clone()),
    };
    let mut node = Node::new(item.name.clone(), Look::Icon(item.icon.clone()), act);
    node.marker = item.app.is_none();
    node.running = !mine.is_empty();
    // Appli ouverte : ses fenêtres, puis de quoi en ouvrir une autre
    if !mine.is_empty() {
        node.children = mine
            .iter()
            .map(|(id, _, title, _)| {
                let name = if title.is_empty() { &item.name } else { title };
                Node::new(name.clone(), Look::Icon(item.icon.clone()), Act::Focus(*id))
            })
            .collect();
        node.children.push(Node::new(
            "Nouvelle fenêtre",
            Look::Glyph(icon::PLUS),
            Act::Run(item.command.clone()),
        ));
    }
    node
}

/// Notifications pas encore vues, et « Ne pas déranger »
fn notifications() -> (usize, bool) {
    let dnd = std::env::var_os("HOME").is_some_and(|h| {
        std::path::Path::new(&h)
            .join(".local/state/odysseus/dnd")
            .exists()
    });
    let unseen = std::env::var_os("XDG_RUNTIME_DIR")
        .and_then(|d| {
            std::fs::read_to_string(
                std::path::Path::new(&d).join("odysseus-notifications-non-vues"),
            )
            .ok()
        })
        .and_then(|t| t.trim().parse().ok())
        .unwrap_or(0);
    (unseen, dnd)
}

/// Le statut : ce qui demande l'attention d'abord, puis les panneaux du
/// bureau, puis les icônes des applis
pub fn status(tray: Vec<sni::Item>) -> Node {
    let (unseen, dnd) = notifications();
    let mut bell = Node::new(
        "Notifications",
        Look::Glyph(if dnd { icon::BELL_OFF } else { icon::BELL }),
        Act::Sibling("odysseus-notify", &["--center", "show"]),
    );
    if unseen > 0 && !dnd {
        bell.badge = Some(Badge {
            text: unseen.min(99).to_string(),
            urgent: false,
        });
    }
    let pending = updates::load()
        .map(|r| updates::pending(&r))
        .unwrap_or_default();
    let mut upd = Node::new(
        "Mises à jour",
        Look::Glyph(icon::ARROW_DOWN_CIRCLE),
        Act::Sibling("odysseus-reglages", &["mises-a-jour"]),
    );
    if !pending.is_empty() {
        let security = pending.iter().any(|u| u.level == updates::Level::Security);
        upd.badge = Some(Badge {
            text: if security {
                "!".into()
            } else {
                pending.len().min(99).to_string()
            },
            urgent: security,
        });
    }
    let mut fixed = vec![
        bell,
        upd,
        Node::new(
            "Contrôle",
            Look::Glyph(icon::CONTROLS),
            Act::Sibling("odysseus-control", &[]),
        ),
        Node::new(
            "Moniteur",
            Look::Glyph(icon::GAUGE),
            Act::Sibling("odysseus-moniteur", &[]),
        ),
        Node::new(
            "Assistant",
            Look::Glyph(icon::SPARKLES),
            Act::Sibling("odysseus-assistant", &[]),
        ),
    ];
    // Par urgence : rouge, puis badge, puis le reste (ordre gardé)
    fixed.sort_by_key(|n| match &n.badge {
        Some(b) if b.urgent => 0,
        Some(_) => 1,
        None => 2,
    });
    let mut node = Node::new("Statut", Look::Status, Act::Open);
    node.children = fixed;
    set_tray(&mut node, tray);
    node
}

/// Les icônes des applis, après les entrées du bureau (celles qui demandent
/// l'attention d'abord) ; le badge du statut est le plus urgent de tous
pub fn set_tray(status: &mut Node, tray: Vec<sni::Item>) {
    status.children.retain(|c| c.menu_key.is_none());
    let mut items: Vec<Node> = tray
        .into_iter()
        .map(|it| {
            let name = if it.title.is_empty() {
                "Appli".to_string()
            } else {
                it.title
            };
            let mut n = Node::new(name, Look::Image(it.icon), Act::Tray(it.key.clone(), 0, 0));
            n.menu_key = Some(it.key);
            if it.attention {
                n.badge = Some(Badge {
                    text: String::new(),
                    urgent: false,
                });
            }
            n
        })
        .collect();
    items.sort_by_key(|n| n.badge.is_none());
    status.children.extend(items);
    status.badge = status
        .children
        .iter()
        .filter_map(|c| c.badge.as_ref())
        .min_by_key(|b| (!b.urgent, b.text.is_empty()))
        .cloned();
}

/// Le menu d'une icône d'appli, en places (séparateurs et lignes cachées ôtés)
pub fn menu(key: &str, entries: Vec<sni::MenuEntry>) -> Vec<Node> {
    entries
        .into_iter()
        .filter(|e| !e.separator && !e.label.is_empty())
        .map(|e| {
            let act = if e.header || !e.enabled {
                Act::Nothing
            } else {
                Act::TrayMenu(key.to_string(), e.id)
            };
            let mut n = Node::new(e.label, Look::Label, act);
            n.enabled = e.enabled && !e.header;
            n.toggle = e.toggle;
            n
        })
        .collect()
}
