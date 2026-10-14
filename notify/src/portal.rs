//! Portail freedesktop réduit à l'essentiel : org.freedesktop.portal.Settings,
//! espace « org.freedesktop.appearance ». Firefox et les applications GTK 4 /
//! libadwaita y lisent la préférence claire ou sombre (color-scheme) et la
//! couleur d'accent, et suivent ses changements (signal SettingChanged).
//! Les valeurs viennent du thème d'odysseus : Odyssée claire → clair, sinon
//! sombre. Si un vrai xdg-desktop-portal tient déjà le nom, on s'efface.

use std::{os::fd::RawFd, time::Duration};

use dbus::{
    Message,
    arg::{RefArg, Variant},
    channel::{BusType, Channel},
};

const NAME: &str = "org.freedesktop.portal.Desktop";
const PATH: &str = "/org/freedesktop/portal/desktop";
const IFACE: &str = "org.freedesktop.portal.Settings";
const APPEARANCE: &str = "org.freedesktop.appearance";

const INTROSPECTION: &str = r#"<!DOCTYPE node PUBLIC "-//freedesktop//DTD D-BUS Object Introspection 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/introspect.dtd">
<node>
 <interface name="org.freedesktop.portal.Settings">
  <method name="ReadAll"><arg type="as" name="namespaces" direction="in"/><arg type="a{sa{sv}}" name="value" direction="out"/></method>
  <method name="Read"><arg type="s" name="namespace" direction="in"/><arg type="s" name="key" direction="in"/><arg type="v" name="value" direction="out"/></method>
  <method name="ReadOne"><arg type="s" name="namespace" direction="in"/><arg type="s" name="key" direction="in"/><arg type="v" name="value" direction="out"/></method>
  <signal name="SettingChanged"><arg type="s" name="namespace"/><arg type="s" name="key"/><arg type="v" name="value"/></signal>
  <property name="version" type="u" access="read"/>
 </interface>
 <interface name="org.freedesktop.DBus.Properties">
  <method name="Get"><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="v" direction="out"/></method>
  <method name="GetAll"><arg type="s" direction="in"/><arg type="a{sv}" direction="out"/></method>
 </interface>
 <interface name="org.freedesktop.DBus.Introspectable">
  <method name="Introspect"><arg direction="out" type="s"/></method>
 </interface>
</node>"#;

pub struct Portal {
    channel: Channel,
}

/// 1 : préférer le sombre, 2 : préférer le clair
fn color_scheme() -> u32 {
    if common::theme::palette().ink == 0xffffff {
        1
    } else {
        2
    }
}

/// Couleur d'accent, composantes 0–1
fn accent() -> (f64, f64, f64) {
    let a = common::theme::palette().accent;
    (a.r as f64, a.g as f64, a.b as f64)
}

fn value(key: &str) -> Option<Box<dyn RefArg>> {
    match key {
        "color-scheme" => Some(Box::new(color_scheme())),
        "accent-color" => Some(Box::new(accent())),
        "contrast" => Some(Box::new(0u32)),
        _ => None,
    }
}

impl Portal {
    pub fn connect() -> Option<Self> {
        let mut channel = Channel::get_private(BusType::Session).ok()?;
        channel.set_watch_enabled(true);
        let request = Message::new_method_call(
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "RequestName",
        )
        .ok()?
        .append2(NAME, 4u32);
        let reply = channel
            .send_with_reply_and_block(request, Duration::from_secs(5))
            .ok()?;
        matches!(reply.read1::<u32>(), Ok(1) | Ok(4)).then_some(Self { channel })
    }

    pub fn fd(&self) -> RawFd {
        self.channel.watch().fd
    }

    fn reply(&self, msg: Message) {
        let _ = self.channel.send(msg);
        self.channel.flush();
    }

    fn not_found(&self, msg: &Message) {
        self.reply(msg.error(
            &"org.freedesktop.portal.Error.NotFound".into(),
            c"réglage inconnu",
        ));
    }

    /// Répond aux demandes en attente
    pub fn poll(&mut self) {
        let _ = self.channel.read_write(Some(Duration::ZERO));
        while let Some(msg) = self.channel.pop_message() {
            if msg.msg_type() != dbus::MessageType::MethodCall {
                continue;
            }
            let member = msg.member().map(|m| m.to_string()).unwrap_or_default();
            match member.as_str() {
                "ReadOne" | "Read" => {
                    let (ns, key): (Option<String>, Option<String>) = msg.get2();
                    match (ns.as_deref(), key.as_deref().and_then(value)) {
                        (Some(APPEARANCE), Some(v)) => {
                            let reply = if member == "Read" {
                                // Ancienne méthode : valeur enveloppée deux fois, comme le vrai portail
                                msg.method_return().append1(Variant(Variant(v)))
                            } else {
                                msg.method_return().append1(Variant(v))
                            };
                            self.reply(reply);
                        }
                        _ => self.not_found(&msg),
                    }
                }
                "ReadAll" => {
                    let wanted: Vec<String> = msg.read1().unwrap_or_default();
                    let mut all: std::collections::HashMap<
                        String,
                        std::collections::HashMap<String, Variant<Box<dyn RefArg>>>,
                    > = std::collections::HashMap::new();
                    let matches = |ns: &str| {
                        wanted.is_empty()
                            || wanted.iter().any(|w| {
                                w == ns
                                    || (w.ends_with('*') && ns.starts_with(w.trim_end_matches('*')))
                            })
                    };
                    if matches(APPEARANCE) {
                        let entry = all.entry(APPEARANCE.into()).or_default();
                        for key in ["color-scheme", "accent-color", "contrast"] {
                            if let Some(v) = value(key) {
                                entry.insert(key.into(), Variant(v));
                            }
                        }
                    }
                    self.reply(msg.method_return().append1(all));
                }
                "Get" => {
                    let (iface, prop): (Option<String>, Option<String>) = msg.get2();
                    if iface.as_deref() == Some(IFACE) && prop.as_deref() == Some("version") {
                        self.reply(msg.method_return().append1(Variant(2u32)));
                    } else {
                        self.not_found(&msg);
                    }
                }
                "GetAll" => {
                    let mut props: std::collections::HashMap<String, Variant<Box<dyn RefArg>>> =
                        std::collections::HashMap::new();
                    props.insert("version".into(), Variant(Box::new(2u32)));
                    self.reply(msg.method_return().append1(props));
                }
                "Introspect" => self.reply(msg.method_return().append1(INTROSPECTION)),
                "Ping" => self.reply(msg.method_return()),
                _ => self.reply(msg.error(
                    &"org.freedesktop.DBus.Error.UnknownMethod".into(),
                    c"méthode inconnue",
                )),
            }
        }
    }

    /// Thème changé : les applications à l'écoute basculent aussitôt
    pub fn theme_changed(&self) {
        for key in ["color-scheme", "accent-color"] {
            if let (Ok(signal), Some(v)) = (
                Message::new_signal(PATH, IFACE, "SettingChanged"),
                value(key),
            ) {
                let _ = self
                    .channel
                    .send(signal.append3(APPEARANCE, key, Variant(v)));
            }
        }
        self.channel.flush();
    }
}
