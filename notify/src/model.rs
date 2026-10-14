//! Notifications : réception, remplacement, expiration, historique.
//!
//! Règles, à la manière de macOS :
//! - une bannière reste 5 s (ou le délai demandé), indéfiniment si critique
//!   ou si l'application demande 0 ;
//! - une bannière expirée rejoint le centre de notifications ; fermée à la
//!   main ou cliquée, elle disparaît ;
//! - « Ne pas déranger » : pas de bannière (sauf critique), l'historique reste ;
//! - les notifications « synchrones » (volume…) se remplacent sur place et ne
//!   vont pas dans l'historique.

use std::time::{Duration, Instant};

use chrono::{DateTime, Local};
use common::draw::Image;

/// Durée d'une bannière ordinaire
pub const BANNER_TIME: Duration = Duration::from_secs(5);
/// Bannière d'indicateur (volume)
pub const OSD_TIME: Duration = Duration::from_millis(1500);
/// Bannières visibles en même temps
pub const MAX_BANNERS: usize = 3;
/// Historique gardé en mémoire
const MAX_HISTORY: usize = 50;

pub struct Hints {
    pub urgency: u8,
    pub value: Option<u8>,
    pub category: Option<String>,
    pub desktop_entry: Option<String>,
    pub synchronous: Option<String>,
    pub transient: bool,
    pub resident: bool,
    pub image_path: Option<String>,
    pub image: Option<Image>,
}

/// Notification telle que reçue par D-Bus
pub struct Incoming {
    pub app_name: String,
    pub replaces_id: u32,
    pub app_icon: String,
    pub summary: String,
    pub body: String,
    pub actions: Vec<(String, String)>,
    pub hints: Hints,
    pub expire_timeout: i32,
}

pub struct Notification {
    pub id: u32,
    pub app_name: String,
    pub summary: String,
    pub body: String,
    /// Actions autres que « default » (boutons), et présence d'une action par défaut
    pub buttons: Vec<(String, String)>,
    pub has_default: bool,
    pub critical: bool,
    pub value: Option<u8>,
    pub synchronous: Option<String>,
    pub transient: bool,
    pub resident: bool,
    pub icon: Option<Image>,
    pub time: DateTime<Local>,
    /// Bannière : apparition, expiration prévue, départ en cours (animation)
    pub appear: Instant,
    pub deadline: Option<Instant>,
    pub leaving: Option<Instant>,
    /// Position verticale affichée (animée vers sa place dans la pile)
    pub y: f32,
}

/// Raison de fermeture, au sens de la spécification
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    Expired = 1,
    Dismissed = 2,
    Closed = 3,
}

#[derive(Default)]
pub struct Store {
    next_id: u32,
    /// Bannières affichées, la plus récente en premier
    pub banners: Vec<Notification>,
    /// Centre de notifications, la plus récente en premier
    pub history: Vec<Notification>,
    pub dnd: bool,
}

impl Store {
    /// Ajoute (ou remplace) une notification ; renvoie son identifiant.
    pub fn add(&mut self, incoming: Incoming) -> u32 {
        let Incoming {
            app_name,
            replaces_id,
            app_icon,
            summary,
            body,
            actions,
            hints,
            expire_timeout,
        } = incoming;
        let critical = hints.urgency >= 2;
        let icon = crate::icons::resolve(
            hints.image,
            hints.image_path.as_deref(),
            &app_icon,
            hints.desktop_entry.as_deref(),
        );
        let has_default = actions.iter().any(|(k, _)| k == "default");
        let buttons: Vec<_> = actions
            .into_iter()
            .filter(|(k, label)| k != "default" && !label.is_empty())
            .take(3)
            .collect();
        let osd = hints.value.is_some() && hints.synchronous.is_some();
        let lifetime = if critical || expire_timeout == 0 {
            None
        } else if osd {
            Some(OSD_TIME)
        } else if expire_timeout > 0 {
            Some(Duration::from_millis(expire_timeout as u64).max(Duration::from_secs(2)))
        } else {
            Some(BANNER_TIME)
        };
        let _ = hints.category;

        // Remplacement : même identifiant, ou même clé « synchrone » (volume)
        let existing = if replaces_id != 0 {
            Some(replaces_id)
        } else {
            hints.synchronous.as_ref().and_then(|key| {
                self.banners
                    .iter()
                    .find(|n| n.synchronous.as_ref() == Some(key))
                    .map(|n| n.id)
            })
        };
        let id = match existing {
            Some(id) => id,
            None => {
                self.next_id = self.next_id.wrapping_add(1).max(1);
                self.next_id
            }
        };
        let now = Instant::now();
        let mut notification = Notification {
            id,
            app_name,
            summary: strip_markup(&summary),
            body: strip_markup(&body),
            buttons,
            has_default,
            critical,
            value: hints.value,
            synchronous: hints.synchronous,
            transient: hints.transient || osd,
            resident: hints.resident,
            icon,
            time: Local::now(),
            appear: now,
            deadline: lifetime.map(|d| now + d),
            leaving: None,
            y: 0.0,
        };
        // Remplacement sur place : pas de nouvelle animation d'apparition
        if let Some(old) = self.banners.iter_mut().find(|n| n.id == id) {
            notification.appear = old.appear;
            notification.y = old.y;
            *old = notification;
            return id;
        }
        if let Some(pos) = self.history.iter().position(|n| n.id == id) {
            self.history.remove(pos);
        }
        if self.dnd && !critical {
            if !notification.transient {
                self.push_history(notification);
            }
        } else {
            self.banners.insert(0, notification);
        }
        id
    }

    fn push_history(&mut self, mut n: Notification) {
        n.leaving = None;
        n.deadline = None;
        self.history.insert(0, n);
        self.history.truncate(MAX_HISTORY);
    }

    /// Bannières arrivées à échéance : elles partent (animation) vers l'historique.
    /// `hovered` : bannière sous le pointeur, dont le délai est suspendu.
    pub fn tick(&mut self, hovered: Option<u32>) {
        let now = Instant::now();
        for (i, n) in self.banners.iter_mut().enumerate() {
            if Some(n.id) == hovered {
                if let Some(d) = n.deadline.as_mut() {
                    *d = (*d).max(now + Duration::from_secs(2));
                }
                continue;
            }
            // Au-delà des bannières visibles, le compte à rebours attend
            if i >= MAX_BANNERS {
                if let Some(d) = n.deadline.as_mut() {
                    *d = (*d).max(now + BANNER_TIME);
                }
                continue;
            }
            if n.leaving.is_none() && n.deadline.is_some_and(|d| now >= d) {
                n.leaving = Some(now);
            }
        }
    }

    /// Retire les bannières dont l'animation de départ est finie ; renvoie
    /// celles qui ont expiré (à signaler et à archiver si besoin).
    pub fn reap(&mut self, leave_time: Duration) -> Vec<u32> {
        let mut expired = Vec::new();
        let mut keep = Vec::new();
        for n in std::mem::take(&mut self.banners) {
            match n.leaving {
                Some(t) if t.elapsed() >= leave_time => {
                    // Une bannière qui part sans avoir été fermée à la main a expiré
                    if n.deadline.is_some_and(|d| Instant::now() >= d) {
                        expired.push(n.id);
                        if !n.transient {
                            self.push_history(n);
                        }
                    }
                }
                _ => keep.push(n),
            }
        }
        self.banners = keep;
        expired
    }

    /// Fermeture à la main ou par l'application : retirée partout.
    pub fn remove(&mut self, id: u32) -> bool {
        let mut found = false;
        for n in self.banners.iter_mut().filter(|n| n.id == id) {
            if n.leaving.is_none() {
                n.leaving = Some(Instant::now());
            }
            // Empêche l'archivage à la fin de l'animation
            n.deadline = None;
            found = true;
        }
        let before = self.history.len();
        self.history.retain(|n| n.id != id);
        found || self.history.len() != before
    }

    /// Notification (bannière ou historique) par identifiant
    pub fn get(&self, id: u32) -> Option<&Notification> {
        self.banners
            .iter()
            .chain(self.history.iter())
            .find(|n| n.id == id)
    }

    /// Historique groupé par application, dans l'ordre d'arrivée des groupes.
    pub fn groups(&self) -> Vec<(String, Vec<&Notification>)> {
        let mut groups: Vec<(String, Vec<&Notification>)> = Vec::new();
        for n in &self.history {
            match groups.iter_mut().find(|(app, _)| *app == n.app_name) {
                Some((_, list)) => list.push(n),
                None => groups.push((n.app_name.clone(), vec![n])),
            }
        }
        groups
    }
}

/// Texte brut : balises retirées (<b>, <a href…>…), entités décodées.
pub fn strip_markup(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for ch in s.chars() {
        match ch {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    let mut decoded = String::with_capacity(out.len());
    let mut rest = out.as_str();
    while let Some(i) = rest.find('&') {
        decoded.push_str(&rest[..i]);
        rest = &rest[i..];
        let end = rest.find(';').filter(|e| *e <= 8);
        let entity = end.map(|e| &rest[1..e]);
        let ch = match entity {
            Some("amp") => Some('&'),
            Some("lt") => Some('<'),
            Some("gt") => Some('>'),
            Some("quot") => Some('"'),
            Some("apos") => Some('\''),
            Some("nbsp") => Some('\u{a0}'),
            Some(e) if e.starts_with("#x") => u32::from_str_radix(&e[2..], 16)
                .ok()
                .and_then(char::from_u32),
            Some(e) if e.starts_with('#') => e[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match (ch, end) {
            (Some(c), Some(e)) => {
                decoded.push(c);
                rest = &rest[e + 1..];
            }
            _ => {
                decoded.push('&');
                rest = &rest[1..];
            }
        }
    }
    decoded.push_str(rest);
    decoded.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::strip_markup;

    #[test]
    fn markup() {
        assert_eq!(
            strip_markup("<b>Salut</b> &amp; <a href=\"x\">lien</a>"),
            "Salut & lien"
        );
        assert_eq!(strip_markup("a &lt; b &#233;t&#xE9;"), "a < b été");
        assert_eq!(strip_markup("R&D"), "R&D");
    }
}
