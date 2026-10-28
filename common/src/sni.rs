//! Icônes des applications (StatusNotifierItem) : les applications qui en
//! veulent une (lanceurs, messageries, Steam…) s'inscrivent auprès du registre
//! org.kde.StatusNotifierWatcher, que la barre tient elle-même. La barre et la
//! roue (place Statut) en sont les hôtes : elles lisent leurs icônes, titres et
//! menus (com.canonical.dbusmenu) et leur transmettent les clics.
//!
//! Tout le travail D-Bus se fait dans un fil à part : une application lente à
//! répondre ne fige jamais l'interface. Le fil envoie la liste des icônes et
//! les menus à afficher ; l'interface lui renvoie les clics.

use std::{
    collections::HashSet,
    sync::mpsc::{self, TryRecvError},
    time::Duration,
};

use crate::draw::Image;
use dbus::{
    Message,
    arg::{ArgType, PropMap, RefArg, Variant},
    channel::{BusType, Channel},
};

/// Où le fil envoie ses nouvelles (liste des icônes, menus)
pub type Sender = Box<dyn Fn(Update) + Send>;

const WATCHER: &str = "org.kde.StatusNotifierWatcher";
const WATCHER_PATH: &str = "/StatusNotifierWatcher";
const ITEM_IFACES: [&str; 2] = [
    "org.kde.StatusNotifierItem",
    "org.freedesktop.StatusNotifierItem",
];
const MENU_IFACE: &str = "com.canonical.dbusmenu";
/// Une application qui ne répond pas en une demi-seconde est passée
const TIMEOUT: Duration = Duration::from_millis(500);
/// Taille des icônes demandées (16 px logiques, tampon à l'échelle 2)
const ICON_PX: u32 = 32;

const INTROSPECTION: &str = r#"<!DOCTYPE node PUBLIC "-//freedesktop//DTD D-BUS Object Introspection 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/introspect.dtd">
<node>
 <interface name="org.kde.StatusNotifierWatcher">
  <method name="RegisterStatusNotifierItem"><arg direction="in" type="s"/></method>
  <method name="RegisterStatusNotifierHost"><arg direction="in" type="s"/></method>
  <property name="RegisteredStatusNotifierItems" type="as" access="read"/>
  <property name="IsStatusNotifierHostRegistered" type="b" access="read"/>
  <property name="ProtocolVersion" type="i" access="read"/>
  <signal name="StatusNotifierItemRegistered"><arg type="s"/></signal>
  <signal name="StatusNotifierItemUnregistered"><arg type="s"/></signal>
  <signal name="StatusNotifierHostRegistered"/>
  <signal name="StatusNotifierHostUnregistered"/>
 </interface>
 <interface name="org.freedesktop.DBus.Properties">
  <method name="Get"><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="v" direction="out"/></method>
  <method name="GetAll"><arg type="s" direction="in"/><arg type="a{sv}" direction="out"/></method>
 </interface>
 <interface name="org.freedesktop.DBus.Introspectable">
  <method name="Introspect"><arg direction="out" type="s"/></method>
 </interface>
</node>"#;

/// Icône affichée dans la barre
#[derive(Clone)]
pub struct Item {
    /// Bus et chemin de l'objet : identifie l'icône dans les commandes
    pub key: String,
    pub title: String,
    pub icon: Option<Image>,
    /// L'application demande l'attention (NeedsAttention)
    pub attention: bool,
}

/// Ligne d'un menu, déjà mise à plat (sous-menus en retrait sous leur titre)
#[derive(Clone)]
pub struct MenuEntry {
    pub id: i32,
    pub label: String,
    pub enabled: bool,
    pub separator: bool,
    /// Case à cocher ou bouton radio, et son état
    pub toggle: Option<bool>,
    /// Titre d'un sous-menu : non cliquable
    pub header: bool,
    pub depth: u8,
}

pub enum Update {
    Items(Vec<Item>),
    Menu {
        key: String,
        entries: Vec<MenuEntry>,
    },
}

pub enum Command {
    /// Clic gauche (coordonnées à l'écran, en logique)
    Activate { key: String, x: i32, y: i32 },
    /// Clic du milieu
    Secondary { key: String, x: i32, y: i32 },
    /// Clic droit : demander le menu
    Menu(String),
    /// Ligne de menu choisie
    MenuEvent { key: String, id: i32 },
}

/// Lance le fil D-Bus ; renvoie de quoi lui passer les clics. `watcher` :
/// tenir le registre s'il est libre (la barre) ; sinon seulement s'inscrire
/// comme hôte auprès de celui qui le tient (la roue, qui ne vit qu'un instant)
pub fn spawn(tx: Sender, watcher: bool) -> mpsc::Sender<Command> {
    let (cmd_tx, cmd_rx) = mpsc::channel();
    std::thread::spawn(move || {
        if let Err(e) = run(tx, cmd_rx, watcher) {
            eprintln!("icônes des applis : arrêt ({e})");
        }
    });
    cmd_tx
}

struct Known {
    key: String,
    /// Nom tel qu'inscrit (renvoyé par RegisteredStatusNotifierItems)
    service: String,
    bus: String,
    /// Nom unique du propriétaire : les signaux viennent de lui
    owner: String,
    path: String,
    iface: &'static str,
    menu: Option<String>,
    item_is_menu: bool,
    passive: bool,
    /// Propriétés lues au moins une fois
    loaded: bool,
    item: Item,
}

struct Host {
    channel: Channel,
    host_name: String,
    /// La barre tient le registre (sinon, un autre programme le tient)
    watcher: bool,
    items: Vec<Known>,
    tx: Sender,
    /// Peut prendre le registre
    may_watch: bool,
}

fn run(tx: Sender, rx: mpsc::Receiver<Command>, may_watch: bool) -> Result<(), String> {
    let channel = Channel::get_private(BusType::Session).map_err(|e| e.to_string())?;
    let mut host = Host {
        channel,
        host_name: format!("org.kde.StatusNotifierHost-{}", std::process::id()),
        watcher: false,
        items: Vec::new(),
        tx,
        may_watch,
    };
    host.add_match("type='signal',sender='org.freedesktop.DBus',member='NameOwnerChanged'");
    for iface in ITEM_IFACES {
        host.add_match(&format!("type='signal',interface='{iface}'"));
    }
    host.add_match(&format!("type='signal',interface='{WATCHER}'"));
    let name = host.host_name.clone();
    host.request_name(&name);
    host.take_watcher();

    loop {
        if host
            .channel
            .read_write(Some(Duration::from_millis(100)))
            .is_err()
        {
            return Err("bus de session perdu".into());
        }
        let mut dirty = HashSet::new();
        let mut changed = false;
        while let Some(msg) = host.channel.pop_message() {
            changed |= host.handle(msg, &mut dirty);
        }
        for key in dirty {
            host.refresh(&key);
            changed = true;
        }
        if changed {
            host.publish();
        }
        loop {
            match rx.try_recv() {
                Ok(cmd) => host.command(cmd),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Ok(()),
            }
        }
    }
}

/// Valeur contenue dans un variant (ou la valeur elle-même)
fn inner(a: &dyn RefArg) -> &dyn RefArg {
    if a.arg_type() == ArgType::Variant {
        a.as_iter().and_then(|mut i| i.next()).unwrap_or(a)
    } else {
        a
    }
}

fn as_bool(a: &dyn RefArg) -> Option<bool> {
    let a = inner(a);
    a.as_u64()
        .or_else(|| a.as_i64().map(|v| v as u64))
        .map(|v| v != 0)
}

fn bytes(a: &dyn RefArg) -> Vec<u8> {
    let a = inner(a);
    a.as_iter()
        .map(|it| it.filter_map(|b| b.as_u64().map(|b| b as u8)).collect())
        .unwrap_or_default()
}

/// IconPixmap : plusieurs tailles, ARGB32 dans l'ordre réseau ; on garde la
/// plus petite au moins aussi grande que voulu (sinon la plus grande).
fn pixmap(a: &dyn RefArg) -> Option<Image> {
    let mut best: Option<(u32, u32, Vec<u8>)> = None;
    for entry in inner(a).as_iter()? {
        let mut f = inner(entry).as_iter()?;
        let w = f.next()?.as_i64()? as u32;
        let h = f.next()?.as_i64()? as u32;
        let data = f.next()?;
        if w == 0 || h == 0 || w > 1024 || h > 1024 {
            continue;
        }
        let better = match &best {
            None => true,
            Some((bw, ..)) => {
                (w >= ICON_PX && (*bw < ICON_PX || w < *bw)) || (*bw < ICON_PX && w > *bw)
            }
        };
        if better {
            best = Some((w, h, bytes(data)));
        }
    }
    let (w, h, raw) = best?;
    if raw.len() < (w * h * 4) as usize {
        return None;
    }
    let mut data = vec![0u8; (w * h * 4) as usize];
    for (d, s) in data
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(raw.as_chunks::<4>().0)
    {
        let a = s[0] as u32;
        d[0] = (s[3] as u32 * a / 255) as u8;
        d[1] = (s[2] as u32 * a / 255) as u8;
        d[2] = (s[1] as u32 * a / 255) as u8;
        d[3] = a as u8;
    }
    Some(Image {
        width: w,
        height: h,
        data,
    })
}

/// Icône par nom : d'abord dans le dossier propre à l'application
/// (IconThemePath), puis dans le thème du système.
fn named_icon(name: &str, theme_path: &str) -> Option<Image> {
    if name.is_empty() {
        return None;
    }
    if !theme_path.is_empty() {
        let dir = std::path::Path::new(theme_path);
        let mut candidates = vec![
            dir.join(format!("{name}.png")),
            dir.join(format!("{name}.svg")),
        ];
        for s in [32, 48, 64, 24, 22, 16, 128, 256] {
            candidates.push(dir.join(format!("hicolor/{s}x{s}/apps/{name}.png")));
        }
        candidates.push(dir.join(format!("hicolor/scalable/apps/{name}.svg")));
        if let Some(img) = candidates
            .iter()
            .filter(|p| p.exists())
            .find_map(|p| crate::icons::load(&p.to_string_lossy(), ICON_PX))
        {
            return Some(img);
        }
    }
    crate::icons::load(name, ICON_PX)
}

/// Libellé dbusmenu : « _ » marque le raccourci clavier, « __ » est un « _ »
fn label(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '_' {
            if chars.peek() == Some(&'_') {
                chars.next();
                out.push('_');
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Nœud de menu (id, propriétés, enfants) → lignes, enfants compris
fn flatten(node: &dyn RefArg, depth: u8, out: &mut Vec<MenuEntry>) {
    let Some(mut f) = inner(node).as_iter() else {
        return;
    };
    let _id = f.next();
    let _props = f.next();
    let Some(children) = f.next().and_then(|c| inner(c).as_iter()) else {
        return;
    };
    for child in children {
        let Some(mut cf) = inner(child).as_iter() else {
            continue;
        };
        let id = cf.next().and_then(|i| inner(i).as_i64()).unwrap_or(0) as i32;
        let mut entry = MenuEntry {
            id,
            label: String::new(),
            enabled: true,
            separator: false,
            toggle: None,
            header: false,
            depth,
        };
        let mut visible = true;
        let mut toggle_type = String::new();
        let mut toggle_state = false;
        let mut submenu = false;
        if let Some(mut props) = cf.next().and_then(|p| inner(p).as_iter()) {
            while let (Some(k), Some(v)) = (props.next(), props.next()) {
                let v = inner(v);
                match k.as_str().unwrap_or("") {
                    "label" => entry.label = label(v.as_str().unwrap_or("")),
                    "enabled" => entry.enabled = as_bool(v).unwrap_or(true),
                    "visible" => visible = as_bool(v).unwrap_or(true),
                    "type" => entry.separator = v.as_str() == Some("separator"),
                    "toggle-type" => toggle_type = v.as_str().unwrap_or("").to_string(),
                    "toggle-state" => toggle_state = v.as_i64() == Some(1),
                    "children-display" => submenu = v.as_str() == Some("submenu"),
                    _ => {}
                }
            }
        }
        if !visible {
            continue;
        }
        if toggle_type == "checkmark" || toggle_type == "radio" {
            entry.toggle = Some(toggle_state);
        }
        let has_children = cf
            .next()
            .and_then(|c| inner(c).as_iter())
            .is_some_and(|mut c| c.next().is_some());
        if submenu || has_children {
            entry.header = true;
            out.push(entry);
            flatten(child, depth.saturating_add(1), out);
        } else {
            out.push(entry);
        }
    }
}

impl Host {
    fn call(&self, msg: Message) -> Result<Message, String> {
        self.channel
            .send_with_reply_and_block(msg, TIMEOUT)
            .map_err(|e| e.message().unwrap_or("erreur").to_string())
    }

    fn send(&self, msg: Message) {
        let _ = self.channel.send(msg);
        self.channel.flush();
    }

    fn bus_call(member: &str) -> Message {
        Message::new_method_call(
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            member,
        )
        .expect("message D-Bus")
    }

    fn add_match(&self, rule: &str) {
        self.send(Self::bus_call("AddMatch").append1(rule));
    }

    /// true si le nom est à nous
    fn request_name(&self, name: &str) -> bool {
        // 4 = DO_NOT_QUEUE
        self.call(Self::bus_call("RequestName").append2(name, 4u32))
            .ok()
            .and_then(|r| r.read1::<u32>().ok())
            .is_some_and(|r| r == 1 || r == 4)
    }

    fn signal(&self, member: &str, service: Option<&str>) {
        if let Ok(mut s) = Message::new_signal(WATCHER_PATH, WATCHER, member) {
            if let Some(service) = service {
                s = s.append1(service);
            }
            self.send(s);
        }
    }

    /// Tenir le registre ; s'il est déjà tenu ailleurs, s'y inscrire comme hôte
    fn take_watcher(&mut self) {
        self.watcher = self.may_watch && self.request_name(WATCHER);
        if self.watcher {
            self.signal("StatusNotifierHostRegistered", None);
            return;
        }
        let Ok(reg) =
            Message::new_method_call(WATCHER, WATCHER_PATH, WATCHER, "RegisterStatusNotifierHost")
        else {
            return;
        };
        let _ = self.call(reg.append1(self.host_name.as_str()));
        let Ok(get) = Message::new_method_call(
            WATCHER,
            WATCHER_PATH,
            "org.freedesktop.DBus.Properties",
            "Get",
        ) else {
            return;
        };
        let services: Vec<String> = self
            .call(get.append2(WATCHER, "RegisteredStatusNotifierItems"))
            .ok()
            .and_then(|r| r.read1::<Variant<Box<dyn RefArg>>>().ok())
            .and_then(|v| {
                v.0.as_iter()
                    .map(|it| it.filter_map(|s| s.as_str().map(String::from)).collect())
            })
            .unwrap_or_default();
        for s in services {
            self.register(&s, None);
        }
        let keys: Vec<String> = self.items.iter().map(|i| i.key.clone()).collect();
        for key in keys {
            self.refresh(&key);
        }
        self.publish();
    }

    /// Inscription d'une icône : nom de bus (objet /StatusNotifierItem), chemin
    /// d'objet seul (sur le bus de l'appelant), ou « bus/chemin »
    fn register(&mut self, service: &str, sender: Option<&str>) -> Option<String> {
        let (bus, path) = if service.starts_with('/') {
            (sender?.to_string(), service.to_string())
        } else if let Some(i) = service.find('/') {
            (service[..i].to_string(), service[i..].to_string())
        } else {
            (service.to_string(), "/StatusNotifierItem".to_string())
        };
        let owner = if bus.starts_with(':') {
            bus.clone()
        } else {
            self.call(Self::bus_call("GetNameOwner").append1(bus.as_str()))
                .ok()
                .and_then(|r| r.read1::<String>().ok())
                .unwrap_or_else(|| bus.clone())
        };
        let key = format!("{bus}{path}");
        let service = if service.starts_with('/') {
            key.clone()
        } else {
            service.to_string()
        };
        self.items.retain(|i| i.key != key);
        self.items.push(Known {
            key: key.clone(),
            service: service.clone(),
            bus,
            owner,
            path,
            iface: ITEM_IFACES[0],
            menu: None,
            item_is_menu: false,
            passive: false,
            loaded: false,
            item: Item {
                key: key.clone(),
                title: String::new(),
                icon: None,
                attention: false,
            },
        });
        Some(service)
    }

    /// Relit les propriétés d'une icône ; l'oublie si elle ne répond plus
    fn refresh(&mut self, key: &str) {
        let Some(idx) = self.items.iter().position(|i| i.key == key) else {
            return;
        };
        let (bus, path) = (self.items[idx].bus.clone(), self.items[idx].path.clone());
        let mut found = None;
        for iface in ITEM_IFACES {
            let Ok(msg) =
                Message::new_method_call(&bus, &path, "org.freedesktop.DBus.Properties", "GetAll")
            else {
                break;
            };
            if let Ok(props) = self
                .call(msg.append1(iface))
                .and_then(|r| r.read1::<PropMap>().map_err(|e| e.to_string()))
                && !props.is_empty()
            {
                found = Some((iface, props));
                break;
            }
        }
        let Some((iface, props)) = found else {
            self.items.remove(idx);
            return;
        };
        let get = |k: &str| props.get(k).map(|v| &*v.0);
        let text = |k: &str| {
            get(k)
                .and_then(|v| inner(v).as_str())
                .unwrap_or("")
                .to_string()
        };
        let status = text("Status");
        let attention = status == "NeedsAttention";
        let theme_path = text("IconThemePath");
        let icon = (attention
            .then(|| {
                named_icon(&text("AttentionIconName"), &theme_path)
                    .or_else(|| get("AttentionIconPixmap").and_then(pixmap))
            })
            .flatten())
        .or_else(|| named_icon(&text("IconName"), &theme_path))
        .or_else(|| get("IconPixmap").and_then(pixmap));
        let mut title = text("Title");
        if title.is_empty() {
            title = text("Id");
        }
        let k = &mut self.items[idx];
        k.iface = iface;
        k.menu = get("Menu")
            .and_then(|v| inner(v).as_str().map(String::from))
            .filter(|m| !m.is_empty() && m != "/");
        k.item_is_menu = get("ItemIsMenu").and_then(as_bool).unwrap_or(false);
        k.passive = status == "Passive";
        k.loaded = true;
        k.item.title = title;
        k.item.icon = icon;
        k.item.attention = attention;
    }

    fn publish(&self) {
        let items = self
            .items
            .iter()
            .filter(|i| i.loaded && !i.passive)
            .map(|i| i.item.clone())
            .collect();
        (self.tx)(Update::Items(items));
    }

    fn remove_where(&mut self, gone: impl Fn(&Known) -> bool) -> bool {
        let removed: Vec<String> = self
            .items
            .iter()
            .filter(|i| gone(i))
            .map(|i| i.service.clone())
            .collect();
        self.items.retain(|i| !gone(i));
        if self.watcher {
            for s in &removed {
                self.signal("StatusNotifierItemUnregistered", Some(s));
            }
        }
        !removed.is_empty()
    }

    /// Traite un message ; renvoie true si la liste des icônes a changé
    fn handle(&mut self, msg: Message, dirty: &mut HashSet<String>) -> bool {
        let member = msg.member().map(|m| m.to_string()).unwrap_or_default();
        let iface = msg.interface().map(|i| i.to_string()).unwrap_or_default();
        match msg.msg_type() {
            dbus::MessageType::MethodCall => {
                self.method(&msg, &member, dirty);
                false
            }
            dbus::MessageType::Signal => match member.as_str() {
                "NameOwnerChanged" => {
                    let (name, _old, new): (String, String, String) = match msg.read3() {
                        Ok(v) => v,
                        Err(_) => return false,
                    };
                    if name == WATCHER && new.is_empty() && !self.watcher && self.may_watch {
                        // Le registre extérieur est parti : on le reprend
                        self.take_watcher();
                    }
                    if new.is_empty() {
                        self.remove_where(|i| i.owner == name || i.bus == name)
                    } else {
                        false
                    }
                }
                "StatusNotifierItemRegistered" if !self.watcher && iface == WATCHER => {
                    if let Ok(s) = msg.read1::<String>()
                        && !self.items.iter().any(|i| i.service == s)
                    {
                        let sender = msg.sender().map(|s| s.to_string());
                        if let Some(service) = self.register(&s, sender.as_deref()) {
                            dirty.extend(
                                self.items
                                    .iter()
                                    .filter(|i| i.service == service)
                                    .map(|i| i.key.clone()),
                            );
                        }
                    }
                    false
                }
                "StatusNotifierItemUnregistered" if !self.watcher && iface == WATCHER => {
                    match msg.read1::<String>() {
                        Ok(s) => self.remove_where(|i| i.service == s),
                        Err(_) => false,
                    }
                }
                m if m.starts_with("New") && ITEM_IFACES.contains(&iface.as_str()) => {
                    let sender = msg.sender().map(|s| s.to_string()).unwrap_or_default();
                    let path = msg.path().map(|p| p.to_string()).unwrap_or_default();
                    dirty.extend(
                        self.items
                            .iter()
                            .filter(|i| i.owner == sender && i.path == path)
                            .map(|i| i.key.clone()),
                    );
                    false
                }
                _ => false,
            },
            _ => false,
        }
    }

    /// Méthodes du registre (quand la barre le tient)
    fn method(&mut self, msg: &Message, member: &str, dirty: &mut HashSet<String>) {
        let reply = match member {
            "RegisterStatusNotifierItem" => {
                let service: String = msg.read1().unwrap_or_default();
                let sender = msg.sender().map(|s| s.to_string());
                // Réponse d'abord : l'application peut attendre avant de répondre à son tour
                self.send(msg.method_return());
                if let Some(service) = self.register(&service, sender.as_deref()) {
                    dirty.extend(
                        self.items
                            .iter()
                            .filter(|i| i.service == service)
                            .map(|i| i.key.clone()),
                    );
                    self.signal("StatusNotifierItemRegistered", Some(&service));
                }
                return;
            }
            "RegisterStatusNotifierHost" | "Ping" => msg.method_return(),
            "Introspect" => msg.method_return().append1(INTROSPECTION),
            "Get" => {
                let (_iface, prop): (String, String) = msg.read2().unwrap_or_default();
                match self.property(&prop) {
                    Some(v) => msg.method_return().append1(Variant(v)),
                    None => msg.error(
                        &"org.freedesktop.DBus.Error.UnknownProperty".into(),
                        c"propriété inconnue",
                    ),
                }
            }
            "GetAll" => {
                let mut props: PropMap = PropMap::new();
                for p in [
                    "RegisteredStatusNotifierItems",
                    "IsStatusNotifierHostRegistered",
                    "ProtocolVersion",
                ] {
                    if let Some(v) = self.property(p) {
                        props.insert(p.into(), Variant(v));
                    }
                }
                msg.method_return().append1(props)
            }
            _ => msg.error(
                &"org.freedesktop.DBus.Error.UnknownMethod".into(),
                c"méthode inconnue",
            ),
        };
        self.send(reply);
    }

    fn property(&self, name: &str) -> Option<Box<dyn RefArg>> {
        match name {
            "RegisteredStatusNotifierItems" => Some(Box::new(
                self.items
                    .iter()
                    .map(|i| i.service.clone())
                    .collect::<Vec<String>>(),
            )),
            "IsStatusNotifierHostRegistered" => Some(Box::new(true)),
            "ProtocolVersion" => Some(Box::new(0i32)),
            _ => None,
        }
    }

    fn command(&mut self, cmd: Command) {
        match cmd {
            Command::Activate { key, x, y } => {
                let Some(k) = self.items.iter().find(|i| i.key == key) else {
                    return;
                };
                let menu_only = k.item_is_menu;
                let activated = !menu_only
                    && Message::new_method_call(&k.bus, &k.path, k.iface, "Activate")
                        .ok()
                        .is_some_and(|m| self.call(m.append2(x, y)).is_ok());
                // Pas d'action principale : le menu en tient lieu
                if !activated {
                    self.menu(&key);
                }
            }
            Command::Secondary { key, x, y } => {
                if let Some(k) = self.items.iter().find(|i| i.key == key)
                    && let Ok(m) =
                        Message::new_method_call(&k.bus, &k.path, k.iface, "SecondaryActivate")
                {
                    self.send(m.append2(x, y));
                }
            }
            Command::Menu(key) => self.menu(&key),
            Command::MenuEvent { key, id } => {
                if let Some(k) = self.items.iter().find(|i| i.key == key)
                    && let Some(menu) = &k.menu
                    && let Ok(m) = Message::new_method_call(&k.bus, menu, MENU_IFACE, "Event")
                {
                    self.send(m.append3(id, "clicked", Variant(0i32)).append1(0u32));
                }
            }
        }
    }

    /// Lit le menu d'une icône et le passe à la barre
    fn menu(&mut self, key: &str) {
        let Some(k) = self.items.iter().find(|i| i.key == key) else {
            return;
        };
        let Some(menu) = k.menu.clone() else {
            return;
        };
        let bus = k.bus.clone();
        if let Ok(m) = Message::new_method_call(&bus, &menu, MENU_IFACE, "AboutToShow") {
            let _ = self.call(m.append1(0i32));
        }
        let Ok(m) = Message::new_method_call(&bus, &menu, MENU_IFACE, "GetLayout") else {
            return;
        };
        let Ok(reply) = self.call(m.append3(0i32, -1i32, Vec::<String>::new())) else {
            return;
        };
        let mut it = reply.iter_init();
        it.next();
        let Some(layout) = it.get_refarg() else {
            return;
        };
        let mut entries = Vec::new();
        flatten(&*layout, 0, &mut entries);
        // Séparateurs en tête, en fin ou doublés : retirés
        let mut clean: Vec<MenuEntry> = Vec::with_capacity(entries.len());
        for e in entries {
            if e.separator && clean.last().is_none_or(|l| l.separator) {
                continue;
            }
            clean.push(e);
        }
        while clean.last().is_some_and(|l| l.separator) {
            clean.pop();
        }
        if !clean.is_empty() {
            (self.tx)(Update::Menu {
                key: key.to_string(),
                entries: clean,
            });
        }
    }
}
