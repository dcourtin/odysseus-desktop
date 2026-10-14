mod compositor;
mod decoration;
pub mod layer_shell;
mod protocols;
mod xdg_shell;

use crate::Odysseus;

//
// Wl Seat
//

use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::output::OutputHandler;
use smithay::wayland::selection::SelectionHandler;
use smithay::wayland::selection::data_device::{
    ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler,
    set_data_device_focus,
};
use smithay::{delegate_data_device, delegate_output, delegate_seat};

impl SeatHandler for Odysseus {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Odysseus> {
        &mut self.seat_state
    }

    fn cursor_image(
        &mut self,
        _seat: &Seat<Self>,
        image: smithay::input::pointer::CursorImageStatus,
    ) {
        self.cursor_status = image;
    }

    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&WlSurface>) {
        let dh = &self.display_handle;
        let client = focused.and_then(|s| dh.get_client(s.id()).ok());
        set_data_device_focus(dh, seat, client);
    }
}

delegate_seat!(Odysseus);

//
// Wl Data Device
//

impl SelectionHandler for Odysseus {
    /// Contenu mis dans le presse-papiers par le compositeur (capture d'écran)
    type SelectionUserData = crate::screenshot::ClipboardData;

    fn send_selection(
        &mut self,
        _ty: smithay::wayland::selection::SelectionTarget,
        _mime_type: String,
        fd: std::os::fd::OwnedFd,
        _seat: Seat<Self>,
        user_data: &Self::SelectionUserData,
    ) {
        // Écriture dans un fil : le client lit à son rythme
        let data = user_data.clone();
        std::thread::spawn(move || {
            use std::io::Write;
            let _ = std::fs::File::from(fd).write_all(&data);
        });
    }
}

impl DataDeviceHandler for Odysseus {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
    }
}

// Glisser-déposer : l'icône fournie par le client suit le pointeur jusqu'au dépôt
impl ClientDndGrabHandler for Odysseus {
    fn started(
        &mut self,
        _source: Option<smithay::reexports::wayland_server::protocol::wl_data_source::WlDataSource>,
        icon: Option<WlSurface>,
        _seat: Seat<Self>,
    ) {
        self.dnd_icon = icon;
    }

    fn dropped(&mut self, _target: Option<WlSurface>, _validated: bool, _seat: Seat<Self>) {
        self.dnd_icon = None;
    }
}
impl ServerDndGrabHandler for Odysseus {}

delegate_data_device!(Odysseus);

//
// Wl Output & Xdg Output
//

impl OutputHandler for Odysseus {}
delegate_output!(Odysseus);
