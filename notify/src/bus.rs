//! Serveur D-Bus org.freedesktop.Notifications (spécification 1.2), plus une
//! petite interface propre au bureau (fr.odysseus.Notifications) pour le
//! centre de notifications et « Ne pas déranger ».
//!
//! Connexion non bloquante : son descripteur est surveillé par la boucle
//! calloop, les messages sont traités dans le fil de l'interface.

use std::{collections::HashMap, time::Duration};

use dbus::{
    Message,
    arg::{PropMap, RefArg, Variant},
    channel::{BusType, Channel},
};

use crate::model::{Hints, Incoming};

pub const NAME: &str = "org.freedesktop.Notifications";
pub const PATH: &str = "/org/freedesktop/Notifications";
pub const OWN_IFACE: &str = "fr.odysseus.Notifications";

/// Requêtes reçues, transmises à l'interface (une à la fois : la taille importe peu).
#[allow(clippy::large_enum_variant)]
pub enum Request {
    Notify(Incoming, Message),
    Close(u32),
    ToggleCenter,
    ShowCenter,
    SetDnd(bool),
    GetDnd(Message),
    GetHistory(Message),
}

pub struct Bus {
    pub channel: Channel,
}

const INTROSPECTION: &str = r#"<!DOCTYPE node PUBLIC "-//freedesktop//DTD D-BUS Object Introspection 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/introspect.dtd">
<node>
 <interface name="org.freedesktop.Notifications">
  <method name="GetCapabilities"><arg direction="out" type="as"/></method>
  <method name="Notify">
   <arg direction="in" type="s"/><arg direction="in" type="u"/><arg direction="in" type="s"/>
   <arg direction="in" type="s"/><arg direction="in" type="s"/><arg direction="in" type="as"/>
   <arg direction="in" type="a{sv}"/><arg direction="in" type="i"/><arg direction="out" type="u"/>
  </method>
  <method name="CloseNotification"><arg direction="in" type="u"/></method>
  <method name="GetServerInformation">
   <arg direction="out" type="s"/><arg direction="out" type="s"/>
   <arg direction="out" type="s"/><arg direction="out" type="s"/>
  </method>
  <signal name="NotificationClosed"><arg type="u"/><arg type="u"/></signal>
  <signal name="ActionInvoked"><arg type="u"/><arg type="s"/></signal>
 </interface>
 <interface name="fr.odysseus.Notifications">
  <method name="ToggleCenter"/>
  <method name="ShowCenter"/>
  <method name="SetDoNotDisturb"><arg direction="in" type="b"/></method>
  <method name="GetDoNotDisturb"><arg direction="out" type="b"/></method>
  <method name="GetHistory"><arg direction="out" type="a(ssss)"/></method>
 </interface>
 <interface name="org.freedesktop.DBus.Introspectable">
  <method name="Introspect"><arg direction="out" type="s"/></method>
 </interface>
</node>"#;

impl Bus {
    /// Se connecte au bus de session et prend le nom des notifications.
    pub fn connect() -> Result<Self, String> {
        let mut channel = Channel::get_private(BusType::Session).map_err(|e| e.to_string())?;
        channel.set_watch_enabled(true);
        let request = Message::new_method_call(
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "RequestName",
        )?
        // 4 = DO_NOT_QUEUE : échouer si un autre démon tient déjà le nom
        .append2(NAME, 4u32);
        let reply = channel
            .send_with_reply_and_block(request, Duration::from_secs(5))
            .map_err(|e| e.to_string())?;
        match reply.read1::<u32>() {
            Ok(1) | Ok(4) => Ok(Self { channel }),
            Ok(_) => Err("un autre démon de notifications est déjà lancé".into()),
            Err(e) => Err(e.to_string()),
        }
    }

    /// Lit les messages en attente et les traduit en requêtes.
    pub fn poll(&mut self) -> Vec<Request> {
        let _ = self.channel.read_write(Some(Duration::ZERO));
        let mut requests = Vec::new();
        while let Some(msg) = self.channel.pop_message() {
            if msg.msg_type() != dbus::MessageType::MethodCall {
                continue;
            }
            let member = msg.member().map(|m| m.to_string()).unwrap_or_default();
            match member.as_str() {
                "GetCapabilities" => {
                    let caps = vec![
                        "body",
                        "actions",
                        "persistence",
                        "icon-static",
                        "body-hyperlinks",
                    ];
                    self.reply(msg.method_return().append1(caps));
                }
                "GetServerInformation" => {
                    self.reply(
                        msg.method_return()
                            .append2("odysseus-notify", "odysseus")
                            .append2(env!("CARGO_PKG_VERSION"), "1.2"),
                    );
                }
                "Notify" => match parse_notify(&msg) {
                    Some(incoming) => requests.push(Request::Notify(incoming, msg)),
                    None => self.reply(msg.error(
                        &"org.freedesktop.DBus.Error.InvalidArgs".into(),
                        c"arguments invalides",
                    )),
                },
                "CloseNotification" => {
                    if let Ok(id) = msg.read1::<u32>() {
                        requests.push(Request::Close(id));
                    }
                    self.reply(msg.method_return());
                }
                "ToggleCenter" => {
                    requests.push(Request::ToggleCenter);
                    self.reply(msg.method_return());
                }
                "ShowCenter" => {
                    requests.push(Request::ShowCenter);
                    self.reply(msg.method_return());
                }
                "SetDoNotDisturb" => {
                    if let Ok(on) = msg.read1::<bool>() {
                        requests.push(Request::SetDnd(on));
                    }
                    self.reply(msg.method_return());
                }
                "GetDoNotDisturb" => requests.push(Request::GetDnd(msg)),
                "GetHistory" => requests.push(Request::GetHistory(msg)),
                "Introspect" => self.reply(msg.method_return().append1(INTROSPECTION)),
                "Ping" => self.reply(msg.method_return()),
                _ => self.reply(msg.error(
                    &"org.freedesktop.DBus.Error.UnknownMethod".into(),
                    c"méthode inconnue",
                )),
            }
        }
        requests
    }

    pub fn reply(&mut self, msg: Message) {
        let _ = self.channel.send(msg);
        self.channel.flush();
    }

    /// Signal NotificationClosed : 1 expirée, 2 fermée par l'utilisateur, 3 par l'application.
    pub fn closed(&mut self, id: u32, reason: u32) {
        if let Ok(signal) = Message::new_signal(PATH, NAME, "NotificationClosed") {
            self.reply(signal.append2(id, reason));
        }
    }

    pub fn action_invoked(&mut self, id: u32, key: &str) {
        if let Ok(signal) = Message::new_signal(PATH, NAME, "ActionInvoked") {
            self.reply(signal.append2(id, key));
        }
    }
}

/// Notify(app_name, replaces_id, app_icon, summary, body, actions, hints, expire_timeout)
fn parse_notify(msg: &Message) -> Option<Incoming> {
    #[allow(clippy::type_complexity)]
    let (app_name, replaces_id, app_icon, summary, body, actions, hints, expire): (
        String,
        u32,
        String,
        String,
        String,
        Vec<String>,
        PropMap,
        i32,
    ) = msg.read_all().ok()?;
    let actions = actions
        .chunks(2)
        .filter(|pair| pair.len() == 2)
        .map(|pair| (pair[0].clone(), pair[1].clone()))
        .collect();
    Some(Incoming {
        app_name,
        replaces_id,
        app_icon,
        summary,
        body,
        actions,
        hints: parse_hints(&hints),
        expire_timeout: expire,
    })
}

fn parse_hints(hints: &HashMap<String, Variant<Box<dyn RefArg>>>) -> Hints {
    let get = |k: &str| hints.get(k).map(|v| &v.0);
    let urgency = get("urgency").and_then(|v| v.as_u64()).unwrap_or(1) as u8;
    let value = get("value")
        .and_then(|v| v.as_i64())
        .map(|v| v.clamp(0, 100) as u8);
    let category = get("category").and_then(|v| v.as_str().map(String::from));
    let desktop_entry = get("desktop-entry").and_then(|v| v.as_str().map(String::from));
    let synchronous = get("x-canonical-private-synchronous")
        .or_else(|| get("synchronous"))
        .and_then(|v| v.as_str().map(String::from));
    let transient = get("transient")
        .and_then(|v| v.as_u64())
        .is_some_and(|v| v != 0)
        || get("transient")
            .and_then(|v| v.as_i64())
            .is_some_and(|v| v != 0);
    let resident = get("resident")
        .and_then(|v| v.as_u64())
        .is_some_and(|v| v != 0);
    let image_path = get("image-path")
        .or_else(|| get("image_path"))
        .and_then(|v| v.as_str().map(String::from));
    let image = ["image-data", "image_data", "icon_data"]
        .iter()
        .find_map(|k| get(k))
        .and_then(|v| parse_image(v.as_ref()));
    Hints {
        urgency,
        value,
        category,
        desktop_entry,
        synchronous,
        transient,
        resident,
        image_path,
        image,
    }
}

/// Image brute (iiibiiay) : largeur, hauteur, pas, alpha, bits, canaux, octets RGB(A).
fn parse_image(v: &dyn RefArg) -> Option<common::draw::Image> {
    let mut it = v.as_iter()?;
    let width = it.next()?.as_i64()? as u32;
    let height = it.next()?.as_i64()? as u32;
    let stride = it.next()?.as_i64()? as usize;
    let alpha = it.next()?.as_u64()? != 0;
    let _bits = it.next()?.as_i64()?;
    let channels = it.next()?.as_i64()? as usize;
    let bytes: Vec<u8> = it
        .next()?
        .as_iter()?
        .filter_map(|b| b.as_u64().map(|b| b as u8))
        .collect();
    if width == 0 || height == 0 || width > 1024 || height > 1024 || channels < 3 {
        return None;
    }
    let mut data = vec![0u8; (width * height * 4) as usize];
    for y in 0..height as usize {
        for x in 0..width as usize {
            let s = y * stride + x * channels;
            if s + channels > bytes.len() {
                return None;
            }
            let a = if alpha { bytes[s + 3] as u32 } else { 255 };
            let d = (y * width as usize + x) * 4;
            data[d] = (bytes[s + 2] as u32 * a / 255) as u8;
            data[d + 1] = (bytes[s + 1] as u32 * a / 255) as u8;
            data[d + 2] = (bytes[s] as u32 * a / 255) as u8;
            data[d + 3] = a as u8;
        }
    }
    Some(common::draw::Image {
        width,
        height,
        data,
    })
}
