//! banc-sni : une icône d'appli d'essai (StatusNotifierItem et son menu
//! com.canonical.dbusmenu), pour voir la place Statut de la roue sans
//! installer d'appli qui en a une.
//!
//! `banc-sni NOM [attention] [JOURNAL]` : s'inscrit auprès du registre
//! (org.kde.StatusNotifierWatcher, tenu par la barre) et note dans JOURNAL
//! les clics reçus (Activate, lignes du menu).

use std::{io::Write, time::Duration};

use dbus::{
    Message, MessageType,
    arg::{PropMap, RefArg, Variant},
    channel::{BusType, Channel},
};

const ITEM: &str = "org.kde.StatusNotifierItem";
const MENU: &str = "com.canonical.dbusmenu";
const PROPS: &str = "org.freedesktop.DBus.Properties";

type Layout = (i32, PropMap, Vec<Variant<Box<dyn RefArg>>>);

fn entry(id: i32, label: &str, extra: &[(&str, Box<dyn RefArg>)]) -> Variant<Box<dyn RefArg>> {
    let mut props = PropMap::new();
    props.insert("label".into(), Variant(Box::new(label.to_string())));
    for (k, v) in extra {
        props.insert(k.to_string(), Variant(v.box_clone()));
    }
    let children: Vec<Variant<Box<dyn RefArg>>> = Vec::new();
    Variant(Box::new((id, props, children)))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let title = args.first().cloned().unwrap_or_else(|| "Synchro".into());
    let attention = args.get(1).is_some_and(|a| a == "attention");
    let mut log: Box<dyn Write> = match args.get(2) {
        Some(p) => Box::new(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(p)
                .expect("journal"),
        ),
        None => Box::new(std::io::sink()),
    };
    let mut channel = Channel::get_private(BusType::Session).expect("bus de session");
    channel.set_watch_enabled(true);
    let me = channel.unique_name().unwrap_or_default().to_string();
    let register = Message::new_method_call(
        "org.kde.StatusNotifierWatcher",
        "/StatusNotifierWatcher",
        "org.kde.StatusNotifierWatcher",
        "RegisterStatusNotifierItem",
    )
    .expect("message")
    .append1(me.as_str());
    channel
        .send_with_reply_and_block(register, Duration::from_secs(2))
        .expect("registre des icônes (la barre tourne-t-elle ?)");

    let props = || {
        let mut p = PropMap::new();
        let mut put = |k: &str, v: Box<dyn RefArg>| {
            p.insert(k.into(), Variant(v));
        };
        put("Id", Box::new(title.to_lowercase()));
        put("Title", Box::new(title.clone()));
        put(
            "Status",
            Box::new(if attention { "NeedsAttention" } else { "Active" }.to_string()),
        );
        put("IconName", Box::new("folder".to_string()));
        put(
            "Menu",
            Box::new(dbus::Path::from("/MenuBar").into_static()),
        );
        put("ItemIsMenu", Box::new(false));
        p
    };

    loop {
        if channel.read_write(Some(Duration::from_millis(200))).is_err() {
            return;
        }
        while let Some(msg) = channel.pop_message() {
            if msg.msg_type() != MessageType::MethodCall {
                continue;
            }
            let iface = msg.interface().map(|i| i.to_string()).unwrap_or_default();
            let member = msg.member().map(|m| m.to_string()).unwrap_or_default();
            let reply = match (iface.as_str(), member.as_str()) {
                (PROPS, "GetAll") => {
                    let asked: String = msg.read1().unwrap_or_default();
                    if asked == ITEM {
                        msg.method_return().append1(props())
                    } else {
                        msg.method_return().append1(PropMap::new())
                    }
                }
                (PROPS, "Get") => {
                    let (_, name): (String, String) = msg.read2().unwrap_or_default();
                    match props().remove(&name) {
                        Some(v) => msg.method_return().append1(v),
                        None => msg.error(&"org.freedesktop.DBus.Error.UnknownProperty".into(), c"inconnue"),
                    }
                }
                (ITEM, m) => {
                    let _ = writeln!(log, "{title} {m}");
                    msg.method_return()
                }
                (MENU, "AboutToShow") => msg.method_return().append1(false),
                (MENU, "GetLayout") => {
                    let separator: Box<dyn RefArg> = Box::new("separator".to_string());
                    let check: Box<dyn RefArg> = Box::new("checkmark".to_string());
                    let on: Box<dyn RefArg> = Box::new(1i32);
                    let children = vec![
                        entry(1, "_Ouvrir le dossier", &[]),
                        entry(2, "Mettre en pause", &[]),
                        entry(3, "", &[("type", separator)]),
                        entry(
                            4,
                            "Démarrer avec la session",
                            &[("toggle-type", check), ("toggle-state", on)],
                        ),
                        entry(5, "Quitter", &[]),
                    ];
                    let root: Layout = (0, PropMap::new(), children);
                    msg.method_return().append2(1u32, root)
                }
                (MENU, "Event") => {
                    let mut it = msg.iter_init();
                    let id: i32 = it.read().unwrap_or(0);
                    let _ = writeln!(log, "{title} menu {id}");
                    msg.method_return()
                }
                _ => msg.error(&"org.freedesktop.DBus.Error.UnknownMethod".into(), c"inconnue"),
            };
            let _ = channel.send(reply);
            channel.flush();
        }
    }
}
