//! Page Mises à jour : ce qu'a trouvé odysseus-veille (chaque jour, ou à la
//! demande), à décider au cas par cas. Rien ne s'installe tout seul :
//! odysseus se met à jour d'un bouton (récupération puis compilation) ; un
//! logiciel du système se prépare avec l'Assistant, qui propose les commandes ;
//! une version peut être écartée (la suivante sera proposée).

use std::{
    path::PathBuf,
    process::{Child, Command},
    time::SystemTime,
};

use common::{
    theme::{self, icon},
    updates::{self, Kind, Level, Report, Update},
};

use super::{App, Hit, Msg, Ui};

#[derive(Default)]
pub struct State {
    pub report: Option<Report>,
    /// À décider (sans les versions écartées), dans l'ordre d'affichage
    pub pending: Vec<Update>,
    stamps: (Option<SystemTime>, Option<SystemTime>),
    /// Vérification en cours (odysseus-veille)
    checking: Option<Child>,
    /// Mise à jour d'odysseus en cours (récupération, compilation)
    upgrading: bool,
}

fn sibling(name: &str) -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(name)))
        .unwrap_or_else(|| PathBuf::from(name))
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into()))
}

impl State {
    /// Relit le rapport s'il a changé (ou la liste des versions écartées)
    pub fn refresh(&mut self) -> bool {
        let stamp = |p: PathBuf| std::fs::metadata(p).and_then(|m| m.modified()).ok();
        let stamps = (
            stamp(updates::report_path()),
            stamp(updates::ignored_path()),
        );
        if stamps == self.stamps && self.report.is_some() {
            return false;
        }
        self.stamps = stamps;
        self.report = updates::load();
        self.pending = self
            .report
            .as_ref()
            .map(updates::pending)
            .unwrap_or_default();
        true
    }

    /// Fin d'une vérification lancée d'ici
    pub fn poll(&mut self) -> bool {
        let done = self
            .checking
            .as_mut()
            .is_some_and(|c| !matches!(c.try_wait(), Ok(None)));
        if done {
            self.checking = None;
            self.stamps = (None, None);
            self.refresh();
        }
        done || self.checking.is_some()
    }

    fn check(&mut self) -> Result<(), String> {
        if self.checking.is_some() {
            return Ok(());
        }
        let child = Command::new(sibling("odysseus-veille"))
            .arg("verifier")
            .spawn()
            .map_err(|e| format!("odysseus-veille : {e}"))?;
        self.checking = Some(child);
        Ok(())
    }
}

/// Récupère les nouveautés d'odysseus et les compile (dans un fil)
fn upgrade_odysseus() -> Result<String, String> {
    let repo = home().join("odysseus-desktop");
    let run = |cmd: &mut Command, what: &str| -> Result<(), String> {
        let out = cmd
            .current_dir(&repo)
            .output()
            .map_err(|e| format!("{what} : {e}"))?;
        if out.status.success() {
            return Ok(());
        }
        let err = String::from_utf8_lossy(&out.stderr);
        let last: Vec<&str> = err.lines().rev().take(3).collect();
        Err(format!(
            "{what} : {}",
            last.into_iter().rev().collect::<Vec<_>>().join(" ")
        ))
    };
    run(
        Command::new("git").args(["pull", "--ff-only", "-q"]),
        "récupération",
    )?;
    run(
        Command::new(home().join(".cargo/bin/cargo")).args(["build", "--release", "-q"]),
        "compilation",
    )?;
    Ok("odysseus est à jour : reconnectez-vous pour en profiter".into())
}

fn severity(s: &str) -> &str {
    match s {
        "Critical" => "critique",
        "High" => "élevée",
        "Medium" => "moyenne",
        "Low" => "faible",
        other => other,
    }
}

/// AAAA-MM-JJ → JJ/MM/AAAA
fn day(date: &str) -> String {
    let parts: Vec<&str> = date.split('-').collect();
    match parts.as_slice() {
        [y, m, d] => format!("{d}/{m}/{y}"),
        _ => date.to_string(),
    }
}

/// Demande faite à l'Assistant pour préparer une mise à jour
fn prompt(u: &Update) -> String {
    let mut q = format!(
        "Prépare la mise à jour de {} ({} → {}) sur cette machine construite depuis les sources (LFS). \
         Appuie-toi sur les notes de construction (documentation « machine ») pour retrouver comment il a été construit, \
         dis-moi ce que change cette version, ce qui dépend de lui et les risques, puis propose les commandes une à une. \
         Ne lance rien sans mon accord.",
        u.name, u.installed, u.available
    );
    if !u.advisories.is_empty() {
        let ids: Vec<&str> = u.advisories.iter().map(|a| a.id.as_str()).collect();
        q.push_str(&format!(" Avis de sécurité LFS : {}.", ids.join(", ")));
    }
    if !u.note.is_empty() {
        q.push_str(&format!(" Note : {}.", u.note));
    }
    q
}

impl App {
    pub(super) fn updates_click(&mut self, hit: Hit) {
        match hit {
            Hit::UpdatesCheck => {
                if let Err(e) = self.updates.check() {
                    self.toast(e);
                }
            }
            Hit::UpdateOdysseus => {
                if self.updates.upgrading {
                    return;
                }
                self.updates.upgrading = true;
                let tx = self.msgs.clone();
                std::thread::spawn(move || {
                    let _ = tx.send(Msg::Upgraded(upgrade_odysseus()));
                });
            }
            Hit::UpdateIgnore(i) => {
                if let Some(u) = self.updates.pending.get(i) {
                    match updates::ignore(&u.name, &u.available) {
                        Ok(()) => {
                            self.toast(format!("{} {} écarté", u.name, u.available));
                            self.updates.refresh();
                        }
                        Err(e) => self.toast(e.to_string()),
                    }
                }
            }
            Hit::UpdatePrepare(i) => {
                let Some(u) = self.updates.pending.get(i).cloned() else {
                    return;
                };
                let result = if u.kind == Kind::Tool {
                    // Outil : sa propre commande, dans un terminal
                    let term = std::env::var("TERMINAL").unwrap_or_else(|_| "foot".into());
                    let script = format!("{} ; echo ; echo 'Entrée pour fermer' ; read _", u.note);
                    Command::new(term).args(["sh", "-c", &script]).spawn()
                } else {
                    Command::new(sibling("odysseus-assistant"))
                        .arg(prompt(&u))
                        .spawn()
                };
                match result {
                    Ok(mut child) => {
                        std::thread::spawn(move || child.wait());
                    }
                    Err(e) => self.toast(e.to_string()),
                }
            }
            _ => {}
        }
    }

    pub(super) fn on_upgraded(&mut self, result: Result<String, String>) {
        self.updates.upgrading = false;
        match result {
            Ok(text) => {
                self.toast(text);
                let _ = self.updates.check();
            }
            Err(e) => self.toast(format!("Mise à jour d'odysseus impossible — {e}")),
        }
    }

    pub(super) fn page_maj(&self, ui: &mut Ui, x: f32, y: f32, w: f32) -> f32 {
        let p = theme::palette();
        let st = &self.updates;
        let mut y = ui.note(
            x,
            y,
            w,
            "La veille passe chaque jour et ne fait que regarder. Rien ne s'installe sans vous : \
             pour chaque logiciel, préparez la mise à jour avec l'Assistant ou écartez cette \
             version (la suivante sera proposée).",
        ) + 10.0;

        // Dernière vérification
        ui.card(x, y, w, 64.0);
        ui.icon(icon::REFRESH, x + 26.0, y + 32.0, 15.0, p.accent);
        let when = match st.report.as_ref() {
            Some(r) => {
                let d = chrono::DateTime::from_timestamp(r.checked, 0)
                    .map(|d| {
                        d.with_timezone(&chrono::Local)
                            .format("%d/%m/%Y à %H:%M")
                            .to_string()
                    })
                    .unwrap_or_default();
                format!("Dernière vérification le {d}")
            }
            None => "Jamais vérifié".into(),
        };
        ui.bold_text(&when, x + 42.0, y + 10.0, 22.0, 13.5, p.text);
        let summary = match st.pending.len() {
            0 => "Tout est à jour".to_string(),
            1 => "1 mise à jour à décider".to_string(),
            n => format!("{n} mises à jour à décider"),
        };
        ui.text(&summary, x + 42.0, y + 32.0, 20.0, 12.0, p.text_dim);
        ui.button(
            x + w - 18.0 - 170.0,
            y + 15.0,
            170.0,
            if st.checking.is_some() {
                "Vérification…"
            } else {
                "Vérifier maintenant"
            },
            false,
            Hit::UpdatesCheck,
        );
        y += 64.0 + 8.0;
        if let Some(r) = st.report.as_ref() {
            for e in &r.errors {
                let line = ui.truncate(&format!("· {e}"), 11.5, w);
                ui.text(&line, x, y, 18.0, 11.5, p.text_dim);
                y += 18.0;
            }
        }
        y += 16.0;

        // odysseus lui-même
        if let Some((i, u)) = st
            .pending
            .iter()
            .enumerate()
            .find(|(_, u)| u.kind == Kind::Odysseus)
        {
            y = ui.section(x, y, "odysseus");
            ui.card(x, y, w, 68.0);
            ui.icon(icon::GIT_BRANCH, x + 26.0, y + 34.0, 15.0, p.accent);
            ui.bold_text(
                &format!("{} → {}", u.installed, u.available),
                x + 42.0,
                y + 12.0,
                22.0,
                13.5,
                p.text,
            );
            let note = ui.truncate(&u.note, 12.0, w - 42.0 - 18.0 - 300.0);
            ui.text(&note, x + 42.0, y + 34.0, 20.0, 12.0, p.text_dim);
            ui.button(
                x + w - 18.0 - 150.0,
                y + 17.0,
                150.0,
                if st.upgrading {
                    "Mise à jour…"
                } else {
                    "Mettre à jour"
                },
                !st.upgrading,
                Hit::UpdateOdysseus,
            );
            ui.button(
                x + w - 18.0 - 150.0 - 12.0 - 110.0,
                y + 17.0,
                110.0,
                "Écarter",
                false,
                Hit::UpdateIgnore(i),
            );
            y += 68.0 + 24.0;
        }

        let groups = [
            (
                "Sécurité",
                "un avis de sécurité concerne la version installée",
            ),
            ("Correctifs", "même branche : en principe sans surprise"),
            ("Nouvelles versions", "à prendre quand vous le voulez"),
            ("Outils", "installés à part du système"),
        ];
        for (g, (title, hint)) in groups.iter().enumerate() {
            let rows: Vec<(usize, &Update)> = st
                .pending
                .iter()
                .enumerate()
                .filter(|(_, u)| match g {
                    0 => u.kind == Kind::System && u.level == Level::Security,
                    1 => u.kind == Kind::System && u.level == Level::Fix,
                    2 => u.kind == Kind::System && u.level == Level::Release,
                    _ => u.kind == Kind::Tool,
                })
                .collect();
            if rows.is_empty() {
                continue;
            }
            y = ui.section(x, y, &format!("{title} ({})", rows.len()));
            y = ui.note(x, y - 4.0, w, hint) + 6.0;
            let rh = 58.0;
            ui.card(x, y, w, rows.len() as f32 * rh);
            for (k, (i, u)) in rows.iter().enumerate() {
                let ry = y + k as f32 * rh;
                if k > 0 {
                    ui.rounded(x + 18.0, ry, w - 36.0, 1.0, 0.0, p.ink(0.06));
                }
                let (glyph, color) = match u.level {
                    Level::Security => (icon::SHIELD_ALERT, p.danger),
                    _ => (icon::PACKAGE, p.text_dim),
                };
                ui.icon(glyph, x + 26.0, ry + rh / 2.0, 15.0, color);
                let buttons = 12.0 + 120.0 + 10.0 + 100.0 + 18.0;
                let text_w = w - 42.0 - buttons;
                let head = if u.installed == u.available {
                    format!("{}  {}", u.name, u.installed)
                } else {
                    format!("{}  {} → {}", u.name, u.installed, u.available)
                };
                let head = ui.truncate(&head, 13.5, text_w);
                ui.bold_text(&head, x + 42.0, ry + 9.0, 22.0, 13.5, p.text);
                let detail = if let Some(a) = u.advisories.first() {
                    let more = if u.advisories.len() > 1 {
                        format!(" (+{})", u.advisories.len() - 1)
                    } else {
                        String::new()
                    };
                    format!(
                        "Avis du {} · gravité {}{more}{}",
                        day(&a.date),
                        severity(&a.severity),
                        if u.note.is_empty() {
                            String::new()
                        } else {
                            format!(" · {}", u.note)
                        }
                    )
                } else {
                    u.note.clone()
                };
                let detail = ui.truncate(&detail, 11.5, text_w);
                ui.text(&detail, x + 42.0, ry + 31.0, 18.0, 11.5, p.text_dim);
                let bx = x + w - 18.0 - 120.0;
                ui.button(
                    bx,
                    ry + 12.0,
                    120.0,
                    if u.kind == Kind::Tool {
                        "Mettre à jour"
                    } else {
                        "Préparer"
                    },
                    u.level == Level::Security,
                    Hit::UpdatePrepare(*i),
                );
                ui.button(
                    bx - 10.0 - 100.0,
                    ry + 12.0,
                    100.0,
                    "Écarter",
                    false,
                    Hit::UpdateIgnore(*i),
                );
            }
            y += rows.len() as f32 * rh + 24.0;
        }
        if st.pending.is_empty() && st.report.is_some() {
            ui.card(x, y, w, 64.0);
            ui.icon(icon::CHECK_CIRCLE, x + 26.0, y + 32.0, 15.0, p.success);
            ui.text(
                "Rien à faire : tout est à jour, ou les versions proposées ont été écartées.",
                x + 42.0,
                y,
                64.0,
                13.0,
                p.text_dim,
            );
            y += 64.0;
        }
        y
    }
}
