//! Page Automatismes : les suites d'actions de ~/.config/odysseus/automatismes,
//! leurs déclencheurs et leurs étapes en clair. Un automatisme écrit par
//! l'agent (ou modifié) ne tourne qu'une fois validé ici ; on peut ensuite le
//! mettre en pause, le lancer, l'ouvrir dans l'éditeur ou le jeter.

use std::time::{Duration, Instant};

use common::{
    auto::{self, Automatism, Status},
    theme::{self, icon},
};

use super::{App, Hit, Ui};

impl App {
    pub(super) fn refresh_autos(&mut self) {
        let (all, bad) = auto::load_all();
        self.autos = all
            .into_iter()
            .map(|a| {
                let s = auto::status(&a);
                (a, s)
            })
            .collect();
        self.autos_bad = bad;
    }

    pub(super) fn auto_click(&mut self, hit: Hit) {
        let get = |app: &App, i: usize| app.autos.get(i).map(|(a, _)| a.clone());
        match hit {
            Hit::AutoValidate(i) => {
                if let Some(a) = get(self, i) {
                    match auto::validate(&a) {
                        Ok(()) => self.toast(format!("« {} » validé", a.name)),
                        Err(e) => self.toast(e),
                    }
                }
            }
            Hit::AutoPause(i) => {
                if let Some((a, s)) = self.autos.get(i).cloned() {
                    let _ = auto::set_paused(&a, !s.paused);
                }
            }
            Hit::AutoRun(i) => {
                if let Some(a) = get(self, i) {
                    let exe = std::env::current_exe()
                        .ok()
                        .and_then(|p| p.parent().map(|d| d.join("odysseus-auto")))
                        .unwrap_or_else(|| "odysseus-auto".into());
                    use std::os::unix::process::CommandExt;
                    match std::process::Command::new(exe)
                        .args(["lancer", &a.id])
                        .process_group(0)
                        .spawn()
                    {
                        Ok(mut child) => {
                            std::thread::spawn(move || child.wait());
                            self.toast(format!("« {} » lancé", a.name));
                        }
                        Err(e) => self.toast(format!("odysseus-auto : {e}")),
                    }
                }
            }
            Hit::AutoEdit(i) => {
                if let Some(a) = get(self, i) {
                    self.edit_in_terminal(&a.path(), &format!("{}.json", a.id));
                }
            }
            Hit::AutoDelete(i) => {
                if let Some(a) = get(self, i) {
                    let key = format!("auto:{}", a.id);
                    let armed = self
                        .delete_armed
                        .as_ref()
                        .is_some_and(|(k, at)| *k == key && at.elapsed() < Duration::from_secs(4));
                    if armed {
                        self.delete_armed = None;
                        match common::trash::trash(&a.path()) {
                            Ok(()) => {
                                auto::forget(&a.id);
                                self.toast(format!("« {} » mis à la corbeille", a.name));
                            }
                            Err(e) => self.toast(e),
                        }
                    } else {
                        self.delete_armed = Some((key, Instant::now()));
                    }
                }
            }
            Hit::AutoFolder => {
                let dir = auto::dir();
                let _ = std::fs::create_dir_all(&dir);
                let files = std::env::current_exe()
                    .ok()
                    .and_then(|p| p.parent().map(|d| d.join("files")))
                    .unwrap_or_else(|| "files".into());
                let _ = std::process::Command::new(files).arg(&dir).spawn();
            }
            _ => {}
        }
        self.refresh_autos();
    }

    pub(super) fn page_automatismes(&self, ui: &mut Ui, x: f32, y: f32, w: f32) -> f32 {
        let p = theme::palette();
        let mut y = ui.note(
            x,
            y,
            w,
            "Des suites d'actions du bureau, lancées à la demande, à heure fixe ou quand un fichier \
             arrive dans un dossier. Demandez-en une à l'Assistant (« chaque soir à 22 h, passe en \
             sombre et coupe le son »), ou écrivez-la en JSON. Rien ne tourne avant votre validation : \
             relisez les étapes, surtout les commandes.",
        ) + 6.0;
        ui.button(x, y, 190.0, "Ouvrir le dossier", false, Hit::AutoFolder);
        let hint = auto::tilde(&auto::dir());
        ui.text(&hint, x + 202.0, y, 34.0, 11.5, p.text_dim);
        y += 34.0 + 24.0;

        if self.autos.is_empty() && self.autos_bad.is_empty() {
            ui.card(x, y, w, 64.0);
            ui.text(
                "Aucun automatisme pour l'instant.",
                x + 18.0,
                y,
                64.0,
                13.0,
                p.text_dim,
            );
            return y + 64.0;
        }

        for (i, (a, st)) in self.autos.iter().enumerate() {
            y = self.auto_card(ui, x, y, w, i, a, st) + 16.0;
        }
        for (id, err) in &self.autos_bad {
            let lines = ui.regular.wrap(err, 11.5 * ui.s, (w - 36.0) * ui.s, 3);
            let h = 40.0 + lines.len() as f32 * 15.0 + 10.0;
            ui.card(x, y, w, h);
            ui.bold_text(
                &format!("{id}.json"),
                x + 18.0,
                y + 8.0,
                24.0,
                13.5,
                p.danger,
            );
            for (k, l) in lines.iter().enumerate() {
                ui.text(
                    l,
                    x + 18.0,
                    y + 34.0 + k as f32 * 15.0,
                    15.0,
                    11.5,
                    p.text_dim,
                );
            }
            y += h + 16.0;
        }
        y
    }

    #[allow(clippy::too_many_arguments)]
    fn auto_card(
        &self,
        ui: &mut Ui,
        x: f32,
        y: f32,
        w: f32,
        i: usize,
        a: &Automatism,
        st: &Status,
    ) -> f32 {
        let p = theme::palette();
        let mut steps = Vec::new();
        auto::flatten(&a.steps, 0, &mut steps);
        // Lignes d'étapes repliées sur deux lignes au plus
        let step_w = w - 36.0 - 22.0;
        let wrapped: Vec<(usize, usize, Vec<String>)> = steps
            .iter()
            .enumerate()
            .map(|(n, (d, l))| {
                let indent = *d as f32 * 18.0;
                (
                    n,
                    *d,
                    ui.regular.wrap(l, 12.0 * ui.s, (step_w - indent) * ui.s, 2),
                )
            })
            .collect();
        let steps_h: f32 = wrapped
            .iter()
            .map(|(_, _, l)| l.len() as f32 * 17.0 + 4.0)
            .sum();
        let desc_h = if a.description.is_empty() { 0.0 } else { 18.0 };
        let last_h = if st.last.is_some() { 20.0 } else { 0.0 };
        let h = 16.0 + 24.0 + desc_h + 22.0 + 10.0 + steps_h + 10.0 + last_h + 34.0 + 16.0;
        ui.card(x, y, w, h);

        // Titre et état
        let mut cy = y + 16.0;
        ui.icon(icon::ZAP, x + 26.0, cy + 12.0, 15.0, p.accent);
        ui.bold_text(&a.name, x + 42.0, cy, 24.0, 15.0, p.text);
        let (label, color) = match (st.validated, st.paused) {
            (false, _) if st.changed => ("Modifié — à revalider", p.accent),
            (false, _) => ("À valider", p.accent),
            (true, true) => ("En pause", p.text_dim),
            (true, false) => ("Actif", p.success),
        };
        let pw = ui.measure(label, 11.0) + 18.0;
        ui.rounded(
            x + w - 18.0 - pw,
            cy + 2.0,
            pw,
            20.0,
            10.0,
            color.with_alpha(0.16),
        );
        ui.centered(
            label,
            x + w - 18.0 - pw / 2.0,
            cy + 2.0,
            20.0,
            11.0,
            true,
            color,
        );
        cy += 24.0;
        if !a.description.is_empty() {
            let d = ui.truncate(&a.description, 12.0, w - 36.0);
            ui.text(&d, x + 18.0, cy, 18.0, 12.0, p.text_dim);
            cy += desc_h;
        }
        let triggers = a
            .triggers
            .iter()
            .map(|t| t.label())
            .collect::<Vec<_>>()
            .join(" · ");
        let t = ui.truncate(&triggers, 12.0, w - 36.0 - 22.0);
        ui.icon(icon::CLOCK, x + 25.0, cy + 11.0, 12.0, p.text_soft());
        ui.text(&t, x + 40.0, cy, 22.0, 12.0, p.text_soft());
        cy += 22.0 + 10.0;

        // Étapes, numérotées ; les commandes en accent : ce sont elles qu'on valide
        ui.rounded(
            x + 14.0,
            cy - 4.0,
            w - 28.0,
            steps_h + 8.0,
            10.0,
            p.ink(0.04),
        );
        let mut number = 0;
        for (n, depth, lines) in &wrapped {
            let indent = *depth as f32 * 18.0;
            let line = &steps[*n].1;
            let is_cmd = line.starts_with("Commande :");
            if *depth == 0 && line != "Sinon :" {
                number += 1;
                ui.text(&format!("{number}."), x + 24.0, cy, 17.0, 12.0, p.text_dim);
            }
            for (k, l) in lines.iter().enumerate() {
                ui.text(
                    l,
                    x + 44.0 + indent,
                    cy + k as f32 * 17.0,
                    17.0,
                    12.0,
                    if is_cmd { p.accent } else { p.text },
                );
            }
            cy += lines.len() as f32 * 17.0 + 4.0;
        }
        cy += 10.0;

        if let Some((when, res)) = &st.last {
            let failed = res.starts_with("échec");
            let line = ui.truncate(
                &format!("Dernière exécution : {when} — {res}"),
                11.5,
                w - 36.0,
            );
            ui.text(
                &line,
                x + 18.0,
                cy,
                20.0,
                11.5,
                if failed { p.danger } else { p.text_dim },
            );
            cy += 20.0;
        }

        // Boutons
        let mut bx = x + 18.0;
        if st.validated {
            ui.button(bx, cy, 110.0, "Lancer", true, Hit::AutoRun(i));
            bx += 120.0;
            let auto_triggers = a.triggers.iter().any(|t| *t != auto::Trigger::Manual);
            if auto_triggers {
                ui.button(
                    bx,
                    cy,
                    130.0,
                    if st.paused {
                        "Reprendre"
                    } else {
                        "Mettre en pause"
                    },
                    false,
                    Hit::AutoPause(i),
                );
                bx += 140.0;
            }
        } else {
            ui.button(bx, cy, 130.0, "Valider", true, Hit::AutoValidate(i));
            bx += 140.0;
        }
        ui.button(bx, cy, 110.0, "Modifier", false, Hit::AutoEdit(i));
        let armed = self.delete_armed.as_ref().is_some_and(|(k, at)| {
            *k == format!("auto:{}", a.id) && at.elapsed() < Duration::from_secs(4)
        });
        ui.button(
            x + w - 18.0 - 130.0,
            cy,
            130.0,
            if armed { "Confirmer" } else { "Supprimer" },
            false,
            Hit::AutoDelete(i),
        );
        y + h
    }
}
