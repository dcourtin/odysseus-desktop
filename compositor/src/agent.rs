//! Ce que le compositeur offre aux agents (odysseus-mcp), par l'IPC :
//! la liste des fenêtres de tous les bureaux, la mise au premier plan d'une
//! fenêtre, et une capture écrite à l'endroit demandé (sans passer par
//! ~/Images ni par le presse-papiers).

use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use smithay::{
    desktop::Window,
    wayland::{compositor::with_states, shell::xdg::XdgToplevelSurfaceData},
};

use crate::Odysseus;

/// Identifiant stable d'une fenêtre (attribué à la première demande)
struct WindowId(u64);

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub fn window_id(window: &Window) -> u64 {
    window
        .user_data()
        .get_or_insert(|| WindowId(NEXT_ID.fetch_add(1, Ordering::Relaxed)))
        .0
}

/// Identifiant d'application et titre d'une fenêtre
pub fn app_and_title(window: &Window) -> (String, String) {
    let Some(toplevel) = window.toplevel() else {
        return (String::new(), String::new());
    };
    with_states(toplevel.wl_surface(), |states| {
        states
            .data_map
            .get::<XdgToplevelSurfaceData>()
            .map(|d| {
                let d = d.lock().unwrap();
                (
                    d.app_id.clone().unwrap_or_default(),
                    d.title.clone().unwrap_or_default(),
                )
            })
            .unwrap_or_default()
    })
}

/// Capture demandée par un agent : fichier PNG à écrire, et fenêtre seule
/// (son identifiant) ou écran entier
pub struct AgentCapture {
    pub path: PathBuf,
    pub window: Option<u64>,
}

impl Odysseus {
    /// Toutes les fenêtres : bureau, premier plan, application, titre, position
    pub fn windows_json(&self) -> String {
        let focus = self.seat.get_keyboard().unwrap().current_focus();
        let mut items = Vec::new();
        let active = self.workspaces.active;
        let mut push = |window: &Window, workspace: usize, loc: (i32, i32)| {
            let (app_id, title) = app_and_title(window);
            let size = window.geometry().size;
            let focused = workspace == active
                && window
                    .toplevel()
                    .is_some_and(|t| focus.as_ref() == Some(t.wl_surface()));
            items.push(serde_json::json!({
                "id": window_id(window),
                "app_id": app_id,
                "title": title,
                "workspace": workspace + 1,
                "focused": focused,
                "x": loc.0, "y": loc.1, "width": size.w, "height": size.h,
            }));
        };
        // Bureau actif, du dessous vers le dessus, puis les autres bureaux
        for window in self.space.elements() {
            let loc = self.space.element_location(window).unwrap_or_default();
            push(window, active, (loc.x, loc.y));
        }
        for ws in 0..crate::workspaces::COUNT {
            if ws == active {
                continue;
            }
            for (window, loc) in self.workspaces.hidden(ws) {
                push(window, ws, (loc.x, loc.y));
            }
        }
        serde_json::Value::Array(items).to_string()
    }

    /// Fenêtre d'après son identifiant, avec son bureau (0 = bureau 1)
    pub fn window_by_id(&self, id: u64) -> Option<(Window, usize)> {
        if let Some(w) = self.space.elements().find(|w| window_id(w) == id) {
            return Some((w.clone(), self.workspaces.active));
        }
        (0..crate::workspaces::COUNT).find_map(|ws| {
            self.workspaces
                .hidden(ws)
                .iter()
                .find(|(w, _)| window_id(w) == id)
                .map(|(w, _)| (w.clone(), ws))
        })
    }

    /// Met une fenêtre au premier plan, en allant sur son bureau si besoin
    pub fn focus_by_id(&mut self, id: u64) -> bool {
        let Some((window, ws)) = self.window_by_id(id) else {
            return false;
        };
        if ws != self.workspaces.active {
            self.switch_workspace(ws);
        }
        self.focus_window(&window);
        true
    }
}
