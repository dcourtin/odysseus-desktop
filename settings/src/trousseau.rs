//! Page Trousseau : les trousseaux du service Secret Service (oo7-daemon),
//! ouverts ou verrouillés, et ce que les applications y ont rangé — le nom
//! et l'application, jamais la valeur. Ouvrir (fenêtre odysseus-trousseau),
//! verrouiller, supprimer un secret.

use std::time::{Duration, Instant};

use common::theme::{self, icon};
use dbus::{
    Path,
    blocking::{Connection, stdintf::org_freedesktop_dbus::Properties},
};

use super::{App, Hit, Ui};

const DEST: &str = "org.freedesktop.secrets";
const SERVICE: &str = "org.freedesktop.Secret.Service";
const COLLECTION: &str = "org.freedesktop.Secret.Collection";
const ITEM: &str = "org.freedesktop.Secret.Item";
const TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone)]
pub struct Secret {
    path: Path<'static>,
    label: String,
    /// Application ou schéma (attributs « application », « xdg:schema »)
    origin: String,
    modified: u64,
}

#[derive(Clone)]
pub struct Keyring {
    path: Path<'static>,
    label: String,
    locked: bool,
    secrets: Vec<Secret>,
}

#[derive(Clone)]
pub enum State {
    /// Pas encore lu
    Unknown,
    Ok(Vec<Keyring>),
    Unavailable(String),
}

fn get<T: for<'b> dbus::arg::Get<'b> + 'static>(
    conn: &Connection,
    path: &Path<'_>,
    iface: &str,
    prop: &str,
) -> Option<T> {
    conn.with_proxy(DEST, path.clone(), TIMEOUT)
        .get::<T>(iface, prop)
        .ok()
}

/// Lit les trousseaux et leurs secrets (noms seulement)
pub fn load() -> State {
    let conn = match Connection::new_session() {
        Ok(c) => c,
        Err(e) => return State::Unavailable(format!("bus de session : {e}")),
    };
    let root = Path::from("/org/freedesktop/secrets");
    let collections: Vec<Path<'static>> = match conn
        .with_proxy(DEST, root, TIMEOUT)
        .get(SERVICE, "Collections")
    {
        Ok(c) => c,
        Err(e) => return State::Unavailable(e.message().unwrap_or("service absent").to_string()),
    };
    let mut out = Vec::new();
    // « session » : trousseau temporaire d'oo7, en mémoire seulement
    for path in collections.into_iter().filter(|p| !p.ends_with("/session")) {
        let label: String = get(&conn, &path, COLLECTION, "Label").unwrap_or_default();
        let locked: bool = get(&conn, &path, COLLECTION, "Locked").unwrap_or(true);
        let items: Vec<Path<'static>> = get(&conn, &path, COLLECTION, "Items").unwrap_or_default();
        let mut secrets: Vec<Secret> = items
            .into_iter()
            .map(|p| {
                let attrs: std::collections::HashMap<String, String> =
                    get(&conn, &p, ITEM, "Attributes").unwrap_or_default();
                let origin = ["application", "service", "xdg:schema"]
                    .iter()
                    .find_map(|k| attrs.get(*k).filter(|v| !v.is_empty()).cloned())
                    .unwrap_or_default();
                Secret {
                    label: get(&conn, &p, ITEM, "Label").unwrap_or_default(),
                    modified: get(&conn, &p, ITEM, "Modified").unwrap_or(0),
                    origin,
                    path: p,
                }
            })
            .collect();
        secrets.sort_by_key(|s| std::cmp::Reverse(s.modified));
        out.push(Keyring {
            path,
            label,
            locked,
            secrets,
        });
    }
    State::Ok(out)
}

/// Ouvre (fenêtre de mot de passe) ou verrouille un trousseau
fn set_locked(path: &Path<'static>, lock: bool) -> Result<(), String> {
    let conn = Connection::new_session().map_err(|e| e.to_string())?;
    let proxy = conn.with_proxy(DEST, "/org/freedesktop/secrets", TIMEOUT);
    let (_, prompt): (Vec<Path<'static>>, Path<'static>) = proxy
        .method_call(
            SERVICE,
            if lock { "Lock" } else { "Unlock" },
            (vec![path.clone()],),
        )
        .map_err(|e| e.to_string())?;
    if &*prompt != "/" {
        conn.with_proxy(DEST, prompt, TIMEOUT)
            .method_call::<(), _, _, _>("org.freedesktop.Secret.Prompt", "Prompt", ("",))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn delete(path: &Path<'static>) -> Result<(), String> {
    let conn = Connection::new_session().map_err(|e| e.to_string())?;
    let (prompt,): (Path<'static>,) = conn
        .with_proxy(DEST, path.clone(), TIMEOUT)
        .method_call(ITEM, "Delete", ())
        .map_err(|e| e.to_string())?;
    if &*prompt != "/" {
        conn.with_proxy(DEST, prompt, TIMEOUT)
            .method_call::<(), _, _, _>("org.freedesktop.Secret.Prompt", "Prompt", ("",))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn keyring_name(label: &str) -> String {
    match label.to_lowercase().as_str() {
        "" | "login" | "default keyring" => "Trousseau de session".to_string(),
        _ => label.to_string(),
    }
}

impl App {
    pub(super) fn refresh_keyrings(&mut self) {
        self.keyrings = load();
        self.keyrings_read = Some(Instant::now());
    }

    pub(super) fn keyring_click(&mut self, hit: Hit) {
        let State::Ok(list) = self.keyrings.clone() else {
            return;
        };
        match hit {
            Hit::KeyringLock(i) | Hit::KeyringUnlock(i) => {
                if let Some(k) = list.get(i) {
                    let lock = matches!(hit, Hit::KeyringLock(_));
                    if let Err(e) = set_locked(&k.path, lock) {
                        self.toast(e);
                    }
                }
            }
            Hit::SecretDelete(i, j) => {
                if let Some(s) = list.get(i).and_then(|k| k.secrets.get(j)) {
                    let key = format!("secret:{}", s.path);
                    let armed = self
                        .delete_armed
                        .as_ref()
                        .is_some_and(|(k, at)| *k == key && at.elapsed() < Duration::from_secs(4));
                    if armed {
                        self.delete_armed = None;
                        match delete(&s.path) {
                            Ok(()) => self.toast(format!("« {} » supprimé", s.label)),
                            Err(e) => self.toast(e),
                        }
                    } else {
                        self.delete_armed = Some((key, Instant::now()));
                    }
                }
            }
            _ => {}
        }
        self.refresh_keyrings();
    }

    pub(super) fn page_trousseau(&self, ui: &mut Ui, x: f32, y: f32, w: f32) -> f32 {
        let p = theme::palette();
        let mut y = ui.note(
            x,
            y,
            w,
            "Les applications rangent ici leurs mots de passe et jetons d'accès, chiffrés sur le \
             disque. Le trousseau de session s'ouvre avec votre mot de passe \
             de connexion. Seuls les noms sont montrés, jamais les valeurs ; l'agent n'y a pas accès.",
        ) + 10.0;

        let list = match &self.keyrings {
            State::Ok(list) => list,
            State::Unknown => return y,
            State::Unavailable(e) => {
                ui.card(x, y, w, 76.0);
                ui.bold_text(
                    "Service indisponible",
                    x + 18.0,
                    y + 12.0,
                    24.0,
                    14.0,
                    p.text,
                );
                let msg = ui.truncate(
                    &format!("Le service du trousseau ne répond pas ({e})."),
                    12.0,
                    w - 36.0,
                );
                ui.text(&msg, x + 18.0, y + 40.0, 20.0, 12.0, p.text_dim);
                return y + 76.0;
            }
        };
        if list.is_empty() {
            ui.card(x, y, w, 64.0);
            ui.text(
                "Aucun trousseau : il sera créé au premier secret rangé par une application.",
                x + 18.0,
                y,
                64.0,
                13.0,
                p.text_dim,
            );
            return y + 64.0;
        }

        for (i, k) in list.iter().enumerate() {
            let rows = if k.locked { 0 } else { k.secrets.len().max(1) };
            let h = 16.0 + 26.0 + 20.0 + 12.0 + rows as f32 * 48.0 + 16.0;
            ui.card(x, y, w, h);
            let mut cy = y + 16.0;
            ui.icon(icon::LOCK, x + 26.0, cy + 13.0, 15.0, p.accent);
            ui.bold_text(&keyring_name(&k.label), x + 42.0, cy, 26.0, 15.0, p.text);
            let (pill, color) = if k.locked {
                ("Verrouillé", p.text_dim)
            } else {
                ("Ouvert", p.success)
            };
            let pw = ui.measure(pill, 11.0) + 18.0;
            ui.rounded(
                x + w - 18.0 - 130.0 - 12.0 - pw,
                cy + 3.0,
                pw,
                20.0,
                10.0,
                color.with_alpha(0.16),
            );
            ui.centered(
                pill,
                x + w - 18.0 - 130.0 - 12.0 - pw / 2.0,
                cy + 3.0,
                20.0,
                11.0,
                true,
                color,
            );
            ui.button(
                x + w - 18.0 - 130.0,
                cy - 2.0,
                130.0,
                if k.locked { "Ouvrir" } else { "Verrouiller" },
                k.locked,
                if k.locked {
                    Hit::KeyringUnlock(i)
                } else {
                    Hit::KeyringLock(i)
                },
            );
            cy += 26.0;
            let count = if k.locked {
                "Ouvrez-le pour voir son contenu".to_string()
            } else {
                match k.secrets.len() {
                    0 => "Vide".to_string(),
                    1 => "1 secret".to_string(),
                    n => format!("{n} secrets"),
                }
            };
            ui.text(&count, x + 42.0, cy, 20.0, 12.0, p.text_dim);
            cy += 20.0 + 12.0;

            if !k.locked {
                if k.secrets.is_empty() {
                    ui.text("Rien pour l'instant.", x + 42.0, cy, 48.0, 12.5, p.text_dim);
                }
                for (j, s) in k.secrets.iter().enumerate() {
                    if j > 0 {
                        ui.rounded(x + 18.0, cy, w - 36.0, 1.0, 0.0, p.ink(0.06));
                    }
                    let label = ui.truncate(
                        if s.label.is_empty() {
                            "(sans nom)"
                        } else {
                            &s.label
                        },
                        13.0,
                        w - 36.0 - 160.0,
                    );
                    ui.bold_text(&label, x + 42.0, cy + 6.0, 20.0, 13.0, p.text);
                    let when = if s.modified > 0 {
                        modified(s.modified)
                    } else {
                        String::new()
                    };
                    let detail = [s.origin.as_str(), when.as_str()]
                        .iter()
                        .filter(|v| !v.is_empty())
                        .copied()
                        .collect::<Vec<_>>()
                        .join(" · ");
                    let detail = ui.truncate(&detail, 11.5, w - 36.0 - 160.0);
                    ui.text(&detail, x + 42.0, cy + 26.0, 16.0, 11.5, p.text_dim);
                    let armed = self.delete_armed.as_ref().is_some_and(|(key, at)| {
                        *key == format!("secret:{}", s.path)
                            && at.elapsed() < Duration::from_secs(4)
                    });
                    ui.button(
                        x + w - 18.0 - 120.0,
                        cy + 6.0,
                        120.0,
                        if armed { "Confirmer" } else { "Supprimer" },
                        false,
                        Hit::SecretDelete(i, j),
                    );
                    cy += 48.0;
                }
            }
            y += h + 16.0;
        }
        y
    }
}

/// Date d'une modification (secondes depuis 1970), à la française
fn modified(secs: u64) -> String {
    let days = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
        .saturating_sub(secs))
        / 86_400;
    match days {
        0 => "modifié aujourd'hui".into(),
        1 => "modifié hier".into(),
        n => format!("modifié il y a {n} jours"),
    }
}
