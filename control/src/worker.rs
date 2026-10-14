//! Fil de travail : lit l'état du système (toutes les 2 s, ou sur demande) et
//! exécute les actions, pour que l'interface ne soit jamais bloquée par D-Bus.

use std::{
    sync::mpsc::{Receiver, RecvTimeoutError},
    time::Duration,
};

use dbus::blocking::Connection;
use smithay_client_toolkit::reexports::calloop::channel;

use crate::sys;

#[derive(Clone, Default)]
pub struct Snapshot {
    pub wifi: sys::Wifi,
    pub bt: sys::Bluetooth,
    pub wired: sys::Wired,
    pub sound: sys::Sound,
    pub dnd: bool,
    /// Lumière de nuit : off, on ou auto
    pub night: String,
}

pub enum Act {
    Refresh,
    WifiPower(bool),
    WifiScan,
    WifiConnect(String, Option<String>),
    WifiDisconnect,
    BtPower(bool),
    BtDiscovery(bool),
    BtConnect(String, bool),
    BtDisconnect(String),
    Volume(f32),
    Mute,
    Sink(u32),
    Dnd(bool),
    Night(&'static str),
    /// Méthode de logind : PowerOff, Reboot ou Suspend
    Power(&'static str),
}

pub enum Event {
    State(Box<Snapshot>),
    /// Fin d'une opération longue : message d'erreur éventuel
    Done(Option<String>),
}

pub fn run(rx: Receiver<Act>, tx: channel::Sender<Event>) {
    let system = Connection::new_system().ok();
    let session = Connection::new_session().ok();
    let mut last = Snapshot::default();
    let mut pending = std::collections::VecDeque::new();
    loop {
        let mut act = match pending.pop_front() {
            Some(act) => act,
            None => match rx.recv_timeout(Duration::from_secs(2)) {
                Ok(act) => act,
                Err(RecvTimeoutError::Timeout) => Act::Refresh,
                Err(RecvTimeoutError::Disconnected) => return,
            },
        };
        // Curseur du volume glissé : seule la dernière valeur compte
        if let Act::Volume(_) = act {
            while let Ok(next) = rx.try_recv() {
                match next {
                    Act::Volume(v) => act = Act::Volume(v),
                    other => pending.push_back(other),
                }
            }
        }
        match act {
            Act::Refresh => {}
            Act::WifiPower(on) => {
                if let Some(c) = system.as_ref() {
                    let _ = sys::wifi_set_powered(c, &last.wifi.device, on);
                }
            }
            Act::WifiScan => {
                if let (Some(c), Some(st)) = (system.as_ref(), last.wifi.station.as_ref()) {
                    sys::wifi_scan(c, st);
                }
            }
            Act::WifiDisconnect => {
                if let (Some(c), Some(st)) = (system.as_ref(), last.wifi.station.as_ref()) {
                    let _ = sys::wifi_disconnect(c, st);
                }
            }
            Act::WifiConnect(path, pass) => {
                let tx = tx.clone();
                std::thread::spawn(move || {
                    let r = sys::wifi_connect(&path, pass);
                    let _ = tx.send(Event::Done(r.err()));
                });
            }
            Act::BtPower(on) => {
                if let Some(c) = system.as_ref() {
                    let _ = sys::bt_set_powered(c, &last.bt.adapter, on);
                }
            }
            Act::BtDiscovery(on) => {
                if let Some(c) = system.as_ref()
                    && last.bt.powered
                {
                    sys::bt_discovery(c, &last.bt.adapter, on);
                }
            }
            Act::BtConnect(path, paired) => {
                let tx = tx.clone();
                std::thread::spawn(move || {
                    let r = sys::bt_connect(&path, paired);
                    let _ = tx.send(Event::Done(r.err()));
                });
            }
            Act::BtDisconnect(path) => {
                let tx = tx.clone();
                std::thread::spawn(move || {
                    let r = sys::bt_disconnect(&path);
                    let _ = tx.send(Event::Done(r.err()));
                });
            }
            Act::Volume(v) => {
                sys::set_volume(v);
                // Pas de relecture : le curseur suit déjà la souris
                continue;
            }
            Act::Mute => sys::toggle_mute(),
            Act::Night(mode) => {
                if let Err(e) = sys::compositor_command(&format!("nightlight {mode}")) {
                    let _ = tx.send(Event::Done(Some(format!("Lumière de nuit : {e}"))));
                }
            }
            Act::Sink(id) => sys::set_default_sink(id),
            Act::Dnd(on) => {
                if let Some(c) = session.as_ref() {
                    sys::set_dnd(c, on);
                }
            }
            Act::Power(method) => {
                if let Some(c) = system.as_ref()
                    && let Err(e) = sys::power(c, method)
                {
                    let _ = tx.send(Event::Done(Some(format!("Refusé par le système : {e}"))));
                }
            }
        }
        last = Snapshot {
            wifi: system.as_ref().map(sys::wifi).unwrap_or_default(),
            bt: system.as_ref().map(sys::bluetooth).unwrap_or_default(),
            wired: sys::wired(),
            sound: sys::sound(),
            dnd: session.as_ref().is_some_and(sys::dnd),
            night: sys::night_mode(),
        };
        if tx.send(Event::State(Box::new(last.clone()))).is_err() {
            return;
        }
    }
}
