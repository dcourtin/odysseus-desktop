//! Bureaux 1 à 9. Les fenêtres du bureau actif sont dans `space` ; celles des
//! autres bureaux sont mises de côté avec leur position.

use smithay::{
    desktop::Window,
    utils::{IsAlive, Logical, Point, SERIAL_COUNTER},
};

use crate::Odysseus;

pub const COUNT: usize = 9;

pub struct Workspaces {
    /// Bureau affiché (0 = bureau 1)
    pub active: usize,
    hidden: Vec<Vec<(Window, Point<i32, Logical>)>>,
    /// Dernier état publié aux clients IPC (occupation de chaque bureau)
    last_published: Vec<bool>,
}

impl Default for Workspaces {
    fn default() -> Self {
        Self {
            active: 0,
            hidden: vec![Vec::new(); COUNT],
            last_published: Vec::new(),
        }
    }
}

/// État d'un bureau, tel que publié par l'IPC
pub struct WorkspaceInfo {
    pub num: usize,
    pub focused: bool,
}

impl Odysseus {
    fn occupancy(&self) -> Vec<bool> {
        (0..COUNT)
            .map(|i| {
                if i == self.workspaces.active {
                    self.space.elements().next().is_some()
                } else {
                    !self.workspaces.hidden[i].is_empty()
                }
            })
            .collect()
    }

    /// Bureaux existants (occupés, plus l'actif), comme dans sway.
    pub fn workspace_list(&self) -> Vec<WorkspaceInfo> {
        self.occupancy()
            .into_iter()
            .enumerate()
            .filter(|(i, occupied)| *occupied || *i == self.workspaces.active)
            .map(|(i, _)| WorkspaceInfo {
                num: i + 1,
                focused: i == self.workspaces.active,
            })
            .collect()
    }

    pub fn switch_workspace(&mut self, target: usize) {
        if target >= COUNT || target == self.workspaces.active {
            return;
        }
        // Ranger les fenêtres visibles, du bas vers le haut pour garder l'empilement
        let visible: Vec<(Window, Point<i32, Logical>)> = self
            .space
            .elements()
            .filter_map(|w| self.space.element_location(w).map(|loc| (w.clone(), loc)))
            .collect();
        for (window, _) in &visible {
            self.space.unmap_elem(window);
        }
        // Glissement : le bureau quitté part d'un côté, le nouveau arrive de l'autre
        self.slide = Some(crate::anim::WorkspaceSlide {
            outgoing: visible.clone(),
            direction: if target > self.workspaces.active {
                1.0
            } else {
                -1.0
            },
            start: std::time::Instant::now(),
        });
        self.workspaces.hidden[self.workspaces.active] = visible;

        let incoming = std::mem::take(&mut self.workspaces.hidden[target]);
        let top = incoming.last().map(|(w, _)| w.clone());
        for (window, loc) in incoming {
            self.space.map_element(window, loc, false);
        }
        self.workspaces.active = target;

        // Le focus clavier suit la fenêtre du dessus du nouveau bureau
        let focus = top.and_then(|w| w.toplevel().map(|t| t.wl_surface().clone()));
        let keyboard = self.seat.get_keyboard().unwrap();
        keyboard.set_focus(self, focus, SERIAL_COUNTER.next_serial());
        self.publish_workspaces(true);
    }

    /// Envoie la fenêtre active sur un autre bureau.
    pub fn move_focused_to_workspace(&mut self, target: usize) {
        if target >= COUNT || target == self.workspaces.active {
            return;
        }
        let Some(focused) = self.seat.get_keyboard().unwrap().current_focus() else {
            return;
        };
        let Some(window) = self
            .space
            .elements()
            .find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == &focused))
            .cloned()
        else {
            return;
        };
        let loc = self.space.element_location(&window).unwrap_or_default();
        self.space.unmap_elem(&window);
        self.workspaces.hidden[target].push((window, loc));
        let keyboard = self.seat.get_keyboard().unwrap();
        keyboard.set_focus(self, None, SERIAL_COUNTER.next_serial());
        self.publish_workspaces(true);
    }

    /// Bureau existant suivant (+1) ou précédent (-1), en boucle.
    pub fn cycle_workspace(&mut self, step: isize) {
        let list: Vec<usize> = self.workspace_list().iter().map(|w| w.num - 1).collect();
        let Some(pos) = list.iter().position(|&i| i == self.workspaces.active) else {
            return;
        };
        let next = list[(pos as isize + step).rem_euclid(list.len() as isize) as usize];
        self.switch_workspace(next);
    }

    /// Retire les fenêtres fermées des bureaux cachés et publie tout changement d'occupation.
    pub fn refresh_workspaces(&mut self) {
        for list in &mut self.workspaces.hidden {
            list.retain(|(w, _)| w.alive());
        }
        self.publish_workspaces(false);
    }

    fn publish_workspaces(&mut self, force: bool) {
        let occupancy = self.occupancy();
        if force || occupancy != self.workspaces.last_published {
            self.workspaces.last_published = occupancy;
            crate::ipc::broadcast_workspace_event(self);
        }
    }
}
