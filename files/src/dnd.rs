//! Glisser-déposer : un élément de Fichiers se glisse vers une autre fenêtre
//! (le terminal reçoit son chemin, un navigateur le fichier) ou sur un dossier
//! de la fenêtre (liste, barre latérale, chemin) pour l'y déplacer.

use std::{io::Write, path::Path};

use common::{draw::Canvas, theme};
use smithay_client_toolkit::{
    data_device_manager::{
        WritePipe,
        data_device::{DataDeviceData, DataDeviceHandler},
        data_offer::{DataOfferHandler, DragOffer},
        data_source::DataSourceHandler,
    },
    delegate_data_device,
    reexports::client::{
        Connection, Proxy, QueueHandle,
        protocol::{
            wl_data_device::WlDataDevice, wl_data_device_manager::DndAction,
            wl_data_source::WlDataSource, wl_shm, wl_surface::WlSurface,
        },
    },
    shell::WaylandSurface,
};

use crate::{DRAG_START, Drag, Files, Hit, info};

/// Types annoncés : liste d'URI d'abord (fichiers), texte ensuite (chemin)
const MIMES: [&str; 3] = ["text/uri-list", "text/plain;charset=utf-8", "text/plain"];

impl Files {
    /// Appui sur une ligne puis déplacement : au-delà de quelques pixels, le
    /// glisser commence.
    pub fn pointer_moved(&mut self, x: f64, y: f64) {
        let Some((px, py, row, serial)) = self.press else {
            return;
        };
        if (x - px).hypot(y - py) >= DRAG_START {
            self.press = None;
            self.start_drag(row, serial);
        }
    }

    fn start_drag(&mut self, row: usize, serial: u32) {
        let (Some(manager), Some(device)) = (self.data_manager.as_ref(), self.data_device.as_ref())
        else {
            return;
        };
        let Some(entry) = self.browser.entries.get(row) else {
            return;
        };
        // Un élément de la sélection multiple emmène toute la sélection
        self.collapse = None;
        let (paths, name, is_dir) = if self.browser.multiple() && self.browser.is_picked(row) {
            let sel = self.browser.selection();
            let paths: Vec<_> = sel.iter().map(|e| e.path.clone()).collect();
            let all_dirs = sel.iter().all(|e| e.is_dir);
            let label = format!("{} éléments", paths.len());
            (paths, label, all_dirs)
        } else {
            (vec![entry.path.clone()], entry.name.clone(), entry.is_dir)
        };
        let source =
            manager.create_drag_and_drop_source(&self.qh, MIMES, DndAction::Copy | DndAction::Move);
        let icon = self.compositor.create_surface(&self.qh);
        source.start_drag(device, self.window.wl_surface(), Some(&icon), serial);
        self.draw_drag_icon(&icon, &name, is_dir);
        self.drag = Some(Drag {
            source,
            paths,
            _icon: icon,
        });
    }

    /// Image qui suit le pointeur : pastille avec l'icône et le nom
    fn draw_drag_icon(&mut self, surface: &WlSurface, name: &str, is_dir: bool) {
        let s = self.scale.max(1) as f32;
        let p = theme::palette();
        let label = self.regular.truncate(name, 13.0 * s, 220.0 * s);
        let (w, h) = (
            (self.regular.measure(&label, 13.0 * s) + 52.0 * s).ceil() as u32,
            (34.0 * s) as u32,
        );
        let Ok((buffer, data)) =
            self.pool
                .create_buffer(w as i32, h as i32, w as i32 * 4, wl_shm::Format::Argb8888)
        else {
            return;
        };
        let mut c = Canvas {
            buf: data,
            width: w,
            height: h,
        };
        c.clear();
        let (wf, hf) = (w as f32, h as f32);
        c.fill_rounded(0.0, 0.0, wf, hf, hf / 2.0, p.panel.with_alpha(0.92));
        c.stroke_rounded(0.0, 0.0, wf, hf, hf / 2.0, s, p.accent);
        if is_dir {
            crate::draw_folder_icon(&mut c, 12.0 * s, 9.0 * s, 16.0 * s, p.accent);
        } else {
            crate::draw_file_icon(&mut c, 14.0 * s, 8.0 * s, 12.0 * s, p.text_dim);
        }
        self.regular
            .draw_in(&mut c, &label, 36.0 * s, 0.0, hf, 13.0 * s, p.text);
        surface.set_buffer_scale(s as i32);
        if buffer.attach_to(surface).is_ok() {
            surface.damage_buffer(0, 0, w as i32, h as i32);
            surface.commit();
        }
    }

    /// Dossier sous le pointeur où déposer l'élément glissé, s'il peut y aller
    fn drop_target_at(&self, x: f64, y: f64) -> Option<std::path::PathBuf> {
        let dragged = &self.drag.as_ref()?.paths;
        let target = match self.hit_at(x, y)? {
            Hit::Row(i) => self
                .browser
                .entries
                .get(i)
                .filter(|e| e.is_dir)
                .map(|e| e.path.clone())?,
            Hit::Place(p) | Hit::Crumb(p) => p,
            _ => return None,
        };
        // Ni sur lui-même, ni dans son propre dossier, ni dans un de ses sous-dossiers
        let pointless = dragged
            .iter()
            .any(|d| target.starts_with(d) || d.parent() == Some(target.as_path()));
        (!pointless).then_some(target)
    }

    fn update_drop(&mut self, device: &WlDataDevice, x: f64, y: f64) {
        let target = self.drop_target_at(x, y);
        if let Some(offer) = drag_offer(device) {
            if target.is_some() {
                offer.accept_mime_type(offer.serial, Some(MIMES[0].into()));
                offer.set_actions(DndAction::Move, DndAction::Move);
            } else {
                offer.accept_mime_type(offer.serial, None);
            }
        }
        if target != self.drop_target {
            self.drop_target = target;
            self.draw();
        }
    }

    /// Dépôt de plusieurs éléments : chacun est déplacé ; le message résume
    fn move_into(&mut self, sources: &[std::path::PathBuf], dir: &Path) {
        if let [one] = sources {
            return self.move_one(one, dir);
        }
        let to_trash = common::trash::files_dir().is_some_and(|t| t == dir);
        let mut moved = Vec::new();
        let mut error = None;
        for source in sources {
            match move_path(source, dir, to_trash) {
                Ok(()) => moved.push(source.clone()),
                Err(e) => error = Some(e),
            }
        }
        let n = moved.len();
        let done = if to_trash {
            self.last_trashed = moved;
            format!("{n} éléments mis à la corbeille — Ctrl+Z pour annuler")
        } else {
            format!("{n} éléments déplacés dans {}", dir.display())
        };
        self.browser.reload();
        self.browser.message = Some(match error {
            Some(e) if n == 0 => e,
            Some(e) => format!("{done} ; {e}"),
            None => done,
        });
        self.navigated();
    }

    /// Dépôt sur un dossier : déplacement (corbeille : mise à la corbeille)
    fn move_one(&mut self, source: &Path, dir: &Path) {
        let name = source
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let to_trash = common::trash::files_dir().is_some_and(|t| t == dir);
        let message = if to_trash {
            match common::trash::trash(source) {
                Ok(()) => {
                    self.last_trashed = vec![source.to_path_buf()];
                    format!("« {name} » mis à la corbeille — Ctrl+Z pour annuler")
                }
                Err(e) => e,
            }
        } else if dir.join(&name).exists() {
            format!("« {name} » existe déjà dans {}", dir.display())
        } else {
            // mv : déplace aussi d'un disque à l'autre
            match std::process::Command::new("mv")
                .arg("-n")
                .arg("--")
                .arg(source)
                .arg(dir)
                .output()
            {
                Ok(out) if out.status.success() => {
                    format!("« {name} » déplacé dans {}", dir.display())
                }
                Ok(out) => format!(
                    "Déplacement impossible : {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ),
                Err(e) => format!("Déplacement impossible : {e}"),
            }
        };
        self.browser.reload();
        self.browser.message = Some(message);
        self.navigated();
    }
}

/// Déplace un élément dans `dir` (ou à la corbeille), sans rien écraser
fn move_path(source: &Path, dir: &Path, to_trash: bool) -> Result<(), String> {
    let name = source
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if to_trash {
        return common::trash::trash(source);
    }
    if dir.join(&name).exists() {
        return Err(format!("« {name} » existe déjà dans {}", dir.display()));
    }
    let out = std::process::Command::new("mv")
        .arg("-n")
        .arg("--")
        .arg(source)
        .arg(dir)
        .output()
        .map_err(|e| format!("Déplacement impossible : {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "Déplacement de « {name} » impossible : {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

fn drag_offer(device: &WlDataDevice) -> Option<DragOffer> {
    device.data::<DataDeviceData>()?.drag_offer()
}

impl DataDeviceHandler for Files {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        device: &WlDataDevice,
        x: f64,
        y: f64,
        surface: &WlSurface,
    ) {
        if surface == self.window.wl_surface() {
            self.update_drop(device, x, y);
        }
    }

    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {
        if self.drop_target.take().is_some() {
            self.draw();
        }
    }

    fn motion(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        device: &WlDataDevice,
        x: f64,
        y: f64,
    ) {
        self.update_drop(device, x, y);
    }

    fn selection(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}

    fn drop_performed(&mut self, _: &Connection, _: &QueueHandle<Self>, device: &WlDataDevice) {
        let target = self.drop_target.take();
        let sources = self.drag.as_ref().map(|d| d.paths.clone());
        if let Some(offer) = drag_offer(device) {
            offer.finish();
            offer.destroy();
        }
        match (sources, target) {
            (Some(sources), Some(dir)) => self.move_into(&sources, &dir),
            _ => self.draw(),
        }
    }
}

impl DataOfferHandler for Files {
    fn source_actions(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &mut DragOffer,
        _: DndAction,
    ) {
    }
    fn selected_action(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &mut DragOffer,
        _: DndAction,
    ) {
    }
}

impl DataSourceHandler for Files {
    fn accept_mime(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlDataSource,
        _: Option<String>,
    ) {
    }

    /// L'application qui reçoit le dépôt demande les données
    fn send_request(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        source: &WlDataSource,
        mime: String,
        mut fd: WritePipe,
    ) {
        let Some(drag) = self.drag.as_ref().filter(|d| d.source.inner() == source) else {
            return;
        };
        // Texte : chemins séparés par des espaces (un terminal les reçoit
        // comme arguments)
        let data = if mime == MIMES[0] {
            drag.paths.iter().map(|p| info::uri_list(p)).collect()
        } else {
            drag.paths
                .iter()
                .map(|p| p.to_string_lossy())
                .collect::<Vec<_>>()
                .join(" ")
        };
        let _ = fd.write_all(data.as_bytes());
    }

    fn cancelled(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {
        self.end_drag();
    }

    fn dnd_dropped(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}

    fn dnd_finished(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {
        self.end_drag();
    }

    fn action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: DndAction) {}
}

impl Files {
    fn end_drag(&mut self) {
        self.drag = None;
        if self.drop_target.take().is_some() {
            self.draw();
        }
    }
}

delegate_data_device!(Files);
