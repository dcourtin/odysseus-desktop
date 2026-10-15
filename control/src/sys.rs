//! Accès au système pour le centre de contrôle : Wi-Fi (iwd), Bluetooth
//! (BlueZ), Ethernet (/sys et getifaddrs), son (wpctl / PipeWire), « Ne pas déranger »
//! (odysseus-notify) et alimentation (logind). Tout passe par D-Bus ou par les
//! outils déjà installés : aucun démon supplémentaire.
//!
//! Les opérations longues (connexion Wi-Fi, appairage Bluetooth) tournent dans
//! un fil séparé, avec leur propre connexion D-Bus et un « agent » qui répond
//! aux questions du service (phrase secrète, confirmation).

use std::{collections::HashMap, process::Command, time::Duration};

use dbus::{
    Message, Path,
    arg::{PropMap, RefArg, Variant},
    blocking::{Connection, stdintf::org_freedesktop_dbus::Properties},
    channel::{BusType, Channel},
};

type Managed = HashMap<Path<'static>, HashMap<String, PropMap>>;
const TIMEOUT: Duration = Duration::from_secs(3);

fn managed(conn: &Connection, dest: &str, path: &str) -> Result<Managed, String> {
    let proxy = conn.with_proxy(dest, path, TIMEOUT);
    let (objects,): (Managed,) = proxy
        .method_call(
            "org.freedesktop.DBus.ObjectManager",
            "GetManagedObjects",
            (),
        )
        .map_err(|e| e.to_string())?;
    Ok(objects)
}

fn prop_str(props: &PropMap, key: &str) -> Option<String> {
    props.get(key).and_then(|v| v.0.as_str()).map(String::from)
}

fn prop_bool(props: &PropMap, key: &str) -> bool {
    props
        .get(key)
        .and_then(|v| v.0.as_u64())
        .is_some_and(|v| v != 0)
}

// --- Wi-Fi (iwd) -----------------------------------------------------------

#[derive(Clone, Default)]
pub struct WifiNetwork {
    pub path: String,
    pub name: String,
    /// psk, open, 8021x
    pub kind: String,
    /// Qualité 0–100 (déduite du signal en dBm)
    pub quality: u8,
    pub known: bool,
    pub connected: bool,
}

#[derive(Clone, Default)]
pub struct Wifi {
    /// Une carte Wi-Fi est présente
    pub present: bool,
    pub powered: bool,
    pub device: String,
    pub station: Option<String>,
    pub scanning: bool,
    pub connected: Option<String>,
    pub networks: Vec<WifiNetwork>,
}

pub fn wifi(conn: &Connection) -> Wifi {
    let Ok(objects) = managed(conn, "net.connman.iwd", "/") else {
        return Wifi::default();
    };
    let mut wifi = Wifi::default();
    for (path, ifaces) in &objects {
        if let Some(dev) = ifaces.get("net.connman.iwd.Device") {
            wifi.present = true;
            wifi.powered = prop_bool(dev, "Powered");
            wifi.device = path.to_string();
            if let Some(station) = ifaces.get("net.connman.iwd.Station") {
                wifi.station = Some(path.to_string());
                wifi.scanning = prop_bool(station, "Scanning");
            }
        }
    }
    let mut networks = Vec::new();
    for (path, ifaces) in &objects {
        if let Some(net) = ifaces.get("net.connman.iwd.Network") {
            let connected = prop_bool(net, "Connected");
            let name = prop_str(net, "Name").unwrap_or_default();
            if connected {
                wifi.connected = Some(name.clone());
            }
            networks.push(WifiNetwork {
                path: path.to_string(),
                name,
                kind: prop_str(net, "Type").unwrap_or_default(),
                quality: 0,
                known: net.contains_key("KnownNetwork"),
                connected,
            });
        }
    }
    // Ordre et force du signal : GetOrderedNetworks (signal en centièmes de dBm)
    if let Some(station) = wifi.station.as_ref() {
        let proxy = conn.with_proxy("net.connman.iwd", station.as_str(), TIMEOUT);
        let ordered: Result<(Vec<(Path<'static>, i16)>,), _> =
            proxy.method_call("net.connman.iwd.Station", "GetOrderedNetworks", ());
        if let Ok((list,)) = ordered {
            let mut sorted = Vec::new();
            for (path, signal) in list {
                if let Some(pos) = networks.iter().position(|n| n.path == *path) {
                    let mut n = networks.remove(pos);
                    // -90 dBm → 0 %, -50 dBm → 100 %
                    let dbm = signal as f32 / 100.0;
                    n.quality = (((dbm + 90.0) / 40.0).clamp(0.0, 1.0) * 100.0) as u8;
                    sorted.push(n);
                }
            }
            sorted.extend(networks);
            networks = sorted;
        }
    }
    wifi.networks = networks;
    wifi
}

pub fn wifi_set_powered(conn: &Connection, device: &str, on: bool) -> Result<(), String> {
    conn.with_proxy("net.connman.iwd", device, TIMEOUT)
        .set("net.connman.iwd.Device", "Powered", on)
        .map_err(|e| e.to_string())
}

pub fn wifi_scan(conn: &Connection, station: &str) {
    let proxy = conn.with_proxy("net.connman.iwd", station, TIMEOUT);
    let _: Result<(), _> = proxy.method_call("net.connman.iwd.Station", "Scan", ());
}

pub fn wifi_disconnect(conn: &Connection, station: &str) -> Result<(), String> {
    conn.with_proxy("net.connman.iwd", station, TIMEOUT)
        .method_call("net.connman.iwd.Station", "Disconnect", ())
        .map_err(|e: dbus::Error| e.to_string())
}

const AGENT_PATH: &str = "/fr/odysseus/control/agent";

/// Connexion à un réseau ; la phrase secrète est donnée à iwd par un agent
/// enregistré le temps de la connexion. Bloquant : à appeler dans un fil.
pub fn wifi_connect(network: &str, passphrase: Option<String>) -> Result<(), String> {
    let mut ch = Channel::get_private(BusType::System).map_err(|e| e.to_string())?;
    let agent = passphrase.is_some();
    if agent {
        let register = Message::new_method_call(
            "net.connman.iwd",
            "/net/connman/iwd",
            "net.connman.iwd.AgentManager",
            "RegisterAgent",
        )?
        .append1(Path::from(AGENT_PATH));
        ch.send_with_reply_and_block(register, TIMEOUT)
            .map_err(|e| e.to_string())?;
    }
    let connect = Message::new_method_call(
        "net.connman.iwd",
        network,
        "net.connman.iwd.Network",
        "Connect",
    )?;
    let result = wait_reply(&mut ch, connect, Duration::from_secs(40), |msg| {
        match msg.member().as_deref() {
            Some("RequestPassphrase") | Some("RequestPrivateKeyPassphrase") => match &passphrase {
                Some(p) => Some(msg.method_return().append1(p.as_str())),
                None => Some(msg.error(&"net.connman.iwd.Agent.Error.Canceled".into(), c"annulé")),
            },
            _ => Some(msg.method_return()),
        }
    });
    if agent
        && let Ok(unregister) = Message::new_method_call(
            "net.connman.iwd",
            "/net/connman/iwd",
            "net.connman.iwd.AgentManager",
            "UnregisterAgent",
        )
    {
        let _ = ch.send_with_reply_and_block(unregister.append1(Path::from(AGENT_PATH)), TIMEOUT);
    }
    result.map_err(|e| match e.as_str() {
        s if s.contains("InvalidFormat") || s.contains("Failed") => {
            "Connexion impossible (mot de passe ?)".into()
        }
        s if s.contains("NoAgent") => "Mot de passe requis".into(),
        s => s.to_string(),
    })
}

/// Envoie `msg` et attend sa réponse en répondant aux appels reçus entre-temps
/// (questions de l'agent) avec `answer`.
fn wait_reply(
    ch: &mut Channel,
    msg: Message,
    timeout: Duration,
    answer: impl Fn(&Message) -> Option<Message>,
) -> Result<(), String> {
    let serial = ch
        .send(msg)
        .map_err(|_| "envoi D-Bus impossible".to_string())?;
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        ch.read_write(Some(Duration::from_millis(200)))
            .map_err(|_| "connexion D-Bus perdue".to_string())?;
        while let Some(mut m) = ch.pop_message() {
            match m.msg_type() {
                dbus::MessageType::MethodCall => {
                    if let Some(reply) = answer(&m) {
                        let _ = ch.send(reply);
                    }
                }
                dbus::MessageType::MethodReturn if m.get_reply_serial() == Some(serial) => {
                    return Ok(());
                }
                dbus::MessageType::Error if m.get_reply_serial() == Some(serial) => {
                    return Err(m.get1::<String>().unwrap_or_else(|| {
                        m.as_result()
                            .err()
                            .map(|e| e.to_string())
                            .unwrap_or_default()
                    }));
                }
                _ => {}
            }
        }
        ch.flush();
    }
    Err("délai dépassé".into())
}

// --- Bluetooth (BlueZ) -----------------------------------------------------

#[derive(Clone, Default)]
pub struct BtDevice {
    pub path: String,
    pub name: String,
    pub icon: String,
    pub paired: bool,
    pub connected: bool,
}

#[derive(Clone, Default)]
pub struct Bluetooth {
    pub present: bool,
    pub adapter: String,
    pub powered: bool,
    pub discovering: bool,
    pub devices: Vec<BtDevice>,
}

pub fn bluetooth(conn: &Connection) -> Bluetooth {
    let Ok(objects) = managed(conn, "org.bluez", "/") else {
        return Bluetooth::default();
    };
    let mut bt = Bluetooth::default();
    for (path, ifaces) in &objects {
        if let Some(a) = ifaces.get("org.bluez.Adapter1")
            && !bt.present
        {
            bt.present = true;
            bt.adapter = path.to_string();
            bt.powered = prop_bool(a, "Powered");
            bt.discovering = prop_bool(a, "Discovering");
        }
        if let Some(d) = ifaces.get("org.bluez.Device1") {
            // Appareils sans nom (adresses seules) : sans intérêt pour l'utilisateur
            let Some(name) = prop_str(d, "Alias").or_else(|| prop_str(d, "Name")) else {
                continue;
            };
            if name.replace('-', ":").split(':').count() == 6 && !prop_bool(d, "Paired") {
                continue;
            }
            bt.devices.push(BtDevice {
                path: path.to_string(),
                name,
                icon: prop_str(d, "Icon").unwrap_or_default(),
                paired: prop_bool(d, "Paired"),
                connected: prop_bool(d, "Connected"),
            });
        }
    }
    bt.devices.sort_by(|a, b| {
        b.connected
            .cmp(&a.connected)
            .then(b.paired.cmp(&a.paired))
            .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    bt
}

pub fn bt_set_powered(conn: &Connection, adapter: &str, on: bool) -> Result<(), String> {
    conn.with_proxy("org.bluez", adapter, TIMEOUT)
        .set("org.bluez.Adapter1", "Powered", on)
        .map_err(|e| e.to_string())
}

pub fn bt_discovery(conn: &Connection, adapter: &str, on: bool) {
    let proxy = conn.with_proxy("org.bluez", adapter, TIMEOUT);
    let method = if on {
        "StartDiscovery"
    } else {
        "StopDiscovery"
    };
    let _: Result<(), _> = proxy.method_call("org.bluez.Adapter1", method, ());
}

const BT_AGENT_PATH: &str = "/fr/odysseus/control/btagent";

/// Appairage si besoin (agent « sans clavier ni écran » : confirmations
/// acceptées), confiance, puis connexion. Bloquant : à appeler dans un fil.
pub fn bt_connect(device: &str, paired: bool) -> Result<(), String> {
    let mut ch = Channel::get_private(BusType::System).map_err(|e| e.to_string())?;
    if !paired {
        let register = Message::new_method_call(
            "org.bluez",
            "/org/bluez",
            "org.bluez.AgentManager1",
            "RegisterAgent",
        )?
        .append2(Path::from(BT_AGENT_PATH), "NoInputNoOutput");
        ch.send_with_reply_and_block(register, TIMEOUT)
            .map_err(|e| e.to_string())?;
        let pair = Message::new_method_call("org.bluez", device, "org.bluez.Device1", "Pair")?;
        let result = wait_reply(&mut ch, pair, Duration::from_secs(40), |m| {
            Some(m.method_return())
        });
        if let Ok(unregister) = Message::new_method_call(
            "org.bluez",
            "/org/bluez",
            "org.bluez.AgentManager1",
            "UnregisterAgent",
        ) {
            let _ = ch
                .send_with_reply_and_block(unregister.append1(Path::from(BT_AGENT_PATH)), TIMEOUT);
        }
        if let Err(e) = result
            && !e.contains("AlreadyExists")
        {
            return Err(format!("Appairage impossible ({e})"));
        }
        let trust = Message::new_method_call(
            "org.bluez",
            device,
            "org.freedesktop.DBus.Properties",
            "Set",
        )?
        .append3("org.bluez.Device1", "Trusted", Variant(true));
        let _ = ch.send_with_reply_and_block(trust, TIMEOUT);
    }
    let connect = Message::new_method_call("org.bluez", device, "org.bluez.Device1", "Connect")?;
    ch.send_with_reply_and_block(connect, Duration::from_secs(30))
        .map(|_| ())
        .map_err(|e| format!("Connexion impossible ({})", e.message().unwrap_or("?")))
}

pub fn bt_disconnect(device: &str) -> Result<(), String> {
    let conn = Connection::new_system().map_err(|e| e.to_string())?;
    conn.with_proxy("org.bluez", device, Duration::from_secs(10))
        .method_call("org.bluez.Device1", "Disconnect", ())
        .map_err(|e: dbus::Error| e.to_string())
}

// --- Ethernet --------------------------------------------------------------

#[derive(Clone, Default)]
pub struct Wired {
    pub interface: Option<String>,
    pub up: bool,
    pub address: Option<String>,
}

/// Première interface filaire (ni boucle locale, ni Wi-Fi, ni virtuelle).
pub fn wired() -> Wired {
    let Ok(dir) = std::fs::read_dir("/sys/class/net") else {
        return Wired::default();
    };
    let mut names: Vec<String> = dir
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| {
            let base = std::path::Path::new("/sys/class/net").join(n);
            n != "lo" && !base.join("wireless").exists() && base.join("device").exists()
        })
        .collect();
    names.sort();
    let Some(name) = names.into_iter().next() else {
        return Wired::default();
    };
    let carrier = std::fs::read_to_string(format!("/sys/class/net/{name}/carrier"))
        .is_ok_and(|c| c.trim() == "1");
    Wired {
        address: ipv4(&name),
        interface: Some(name),
        up: carrier,
    }
}

/// Adresse IPv4 d'une interface (lue auprès du système, sans `ip` : il est
/// dans /usr/sbin, hors du PATH de la session)
pub fn ipv4(interface: &str) -> Option<String> {
    common::net::ipv4(interface)
}

// --- Son (PipeWire, via wpctl) ----------------------------------------------

#[derive(Clone, Default)]
pub struct Sink {
    pub id: u32,
    pub name: String,
    pub default: bool,
}

#[derive(Clone, Default)]
pub struct Sound {
    pub available: bool,
    pub volume: f32,
    pub muted: bool,
    pub sinks: Vec<Sink>,
}

pub fn sound() -> Sound {
    let Ok(out) = Command::new("wpctl")
        .args(["get-volume", "@DEFAULT_AUDIO_SINK@"])
        .output()
    else {
        return Sound::default();
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let Some(volume) = text.split_whitespace().nth(1).and_then(|v| v.parse().ok()) else {
        return Sound::default();
    };
    Sound {
        available: true,
        volume,
        muted: text.contains("MUTED"),
        sinks: sinks(),
    }
}

/// Sorties audio, lues dans `wpctl status` (section Audio → Sinks).
fn sinks() -> Vec<Sink> {
    let Ok(out) = Command::new("wpctl").arg("status").output() else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut in_audio = false;
    let mut in_sinks = false;
    let mut list = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_start_matches([' ', '│', '├', '└', '─']);
        if !line.starts_with([' ', '│', '├', '└']) {
            in_audio = line.starts_with("Audio");
            in_sinks = false;
            continue;
        }
        if !in_audio {
            continue;
        }
        if trimmed.starts_with("Sinks:") {
            in_sinks = true;
            continue;
        }
        if trimmed.ends_with(':') {
            in_sinks = false;
            continue;
        }
        if !in_sinks || trimmed.is_empty() {
            continue;
        }
        let default = trimmed.starts_with('*');
        let rest = trimmed.trim_start_matches('*').trim_start();
        let Some((id, name)) = rest.split_once(". ") else {
            continue;
        };
        let Ok(id) = id.trim().parse() else {
            continue;
        };
        let name = name
            .split(" [vol:")
            .next()
            .unwrap_or(name)
            .trim()
            .to_string();
        list.push(Sink { id, name, default });
    }
    list
}

pub fn set_volume(volume: f32) {
    let v = format!("{:.2}", volume.clamp(0.0, 1.0));
    let _ = Command::new("sh")
        .arg("-c")
        .arg(format!(
            "wpctl set-mute @DEFAULT_AUDIO_SINK@ 0; wpctl set-volume @DEFAULT_AUDIO_SINK@ {v}"
        ))
        .status();
}

pub fn toggle_mute() {
    let _ = Command::new("wpctl")
        .args(["set-mute", "@DEFAULT_AUDIO_SINK@", "toggle"])
        .status();
}

pub fn set_default_sink(id: u32) {
    let _ = Command::new("wpctl")
        .args(["set-default", &id.to_string()])
        .status();
}

// --- « Ne pas déranger » (odysseus-notify) ------------------------------------

pub fn dnd(session: &Connection) -> bool {
    let proxy = session.with_proxy(
        "org.freedesktop.Notifications",
        "/org/freedesktop/Notifications",
        Duration::from_millis(500),
    );
    let r: Result<(bool,), _> =
        proxy.method_call("fr.odysseus.Notifications", "GetDoNotDisturb", ());
    r.map(|(on,)| on).unwrap_or(false)
}

pub fn set_dnd(session: &Connection, on: bool) {
    let proxy = session.with_proxy(
        "org.freedesktop.Notifications",
        "/org/freedesktop/Notifications",
        Duration::from_millis(500),
    );
    let _: Result<(), _> = proxy.method_call("fr.odysseus.Notifications", "SetDoNotDisturb", (on,));
}

// --- Alimentation (logind) ---------------------------------------------------

pub fn power(conn: &Connection, method: &str) -> Result<(), String> {
    let proxy = conn.with_proxy(
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        Duration::from_secs(10),
    );
    proxy
        .method_call("org.freedesktop.login1.Manager", method, (false,))
        .map_err(|e: dbus::Error| e.message().unwrap_or("refusé").to_string())
}

// --- Compositeur (IPC compatible sway) -------------------------------------------

/// Envoie une commande au compositeur (socket SWAYSOCK, message RUN_COMMAND).
pub fn compositor_command(command: &str) -> Result<(), String> {
    use std::io::{Read, Write};
    let path = std::env::var("SWAYSOCK").map_err(|_| "compositeur injoignable".to_string())?;
    let mut stream = std::os::unix::net::UnixStream::connect(path).map_err(|e| e.to_string())?;
    let mut msg = b"i3-ipc".to_vec();
    msg.extend_from_slice(&(command.len() as u32).to_ne_bytes());
    msg.extend_from_slice(&0u32.to_ne_bytes());
    msg.extend_from_slice(command.as_bytes());
    stream.write_all(&msg).map_err(|e| e.to_string())?;
    let mut header = [0u8; 14];
    stream.read_exact(&mut header).map_err(|e| e.to_string())?;
    let len = u32::from_ne_bytes([header[6], header[7], header[8], header[9]]) as usize;
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body).map_err(|e| e.to_string())?;
    if String::from_utf8_lossy(&body).contains("\"success\":true") {
        Ok(())
    } else {
        Err("commande refusée".into())
    }
}

/// Mode de la lumière de nuit, tel que le compositeur l'a enregistré.
pub fn night_mode() -> String {
    std::env::var_os("HOME")
        .and_then(|h| {
            std::fs::read_to_string(
                std::path::Path::new(&h).join(".local/state/odysseus/nightlight"),
            )
            .ok()
        })
        .map(|s| s.trim().to_string())
        .filter(|s| s == "on" || s == "auto")
        .unwrap_or_else(|| "off".into())
}
