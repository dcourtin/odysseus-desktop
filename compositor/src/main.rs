#![allow(irrefutable_let_patterns)]

mod agent;
mod anim;
mod capture;
mod closing;
mod cursor;
mod handlers;

mod grabs;
mod input;
mod ipc;
mod lock;
mod night;
mod render;
mod screenshot;
mod snap;
mod state;
mod udev;
mod wallpaper;
mod windows;
mod winit;
mod workspaces;

use smithay::reexports::{
    calloop::EventLoop,
    wayland_server::{Display, DisplayHandle},
};
pub use state::Odysseus;

pub struct CalloopData {
    state: Odysseus,
    display_handle: DisplayHandle,
    /// Backend DRM (mode tty), absent dans une fenêtre winit
    udev: Option<udev::UdevData>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if let Ok(env_filter) = tracing_subscriber::EnvFilter::try_from_default_env() {
        tracing_subscriber::fmt().with_env_filter(env_filter).init();
    } else {
        tracing_subscriber::fmt().init();
    }

    let mut event_loop: EventLoop<'static, CalloopData> = EventLoop::try_new()?;

    let display: Display<Odysseus> = Display::new()?;
    let display_handle = display.handle();
    let state = Odysseus::new(&mut event_loop, display);

    let mut data = CalloopData {
        state,
        display_handle,
        udev: None,
    };

    match ipc::init(&event_loop.handle(), &data.state.socket_name) {
        Ok(path) => data.state.ipc_path = Some(path),
        Err(e) => tracing::warn!("IPC indisponible : {e}"),
    }

    lock::init_idle_timer(&event_loop.handle());

    // Dans une session graphique (sway…) : fenêtre winit ; sur un tty : DRM
    let nested =
        std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var_os("DISPLAY").is_some();
    if nested {
        crate::winit::init_winit(&mut event_loop, &mut data)?;
    } else {
        crate::udev::init_udev(&mut event_loop, &mut data)?;
    }

    // Programme lancé au démarrage : `compositor -c "commande"`, sinon un terminal
    let mut args = std::env::args().skip(1);
    let command = match (args.next().as_deref(), args.next()) {
        (Some("-c") | Some("--command"), Some(command)) => command,
        _ => std::env::var("TERMINAL").unwrap_or_else(|_| "foot".into()),
    };
    data.state.spawn(&command);

    event_loop.run(None, &mut data, move |data| {
        let _ = data.display_handle.flush_clients();
    })?;

    Ok(())
}
