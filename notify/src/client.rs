//! Mode client : commandes envoyées au démon par D-Bus, pour la barre, le
//! compositeur (touches de volume) et les essais.

use std::{path::PathBuf, process::Command, time::Duration};

use dbus::{
    Message,
    arg::{PropMap, Variant},
    channel::{BusType, Channel},
};

use crate::bus::{NAME, OWN_IFACE, PATH};

/// Fichier présent quand « Ne pas déranger » est actif (lu par la barre)
pub fn dnd_file() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".local/state/odysseus/dnd"))
}

const USAGE: &str =
    "usage : odysseus-notify [--center | --dnd on|off|toggle | --send TITRE [TEXTE] | --volume]";

pub fn run(args: &[String]) -> i32 {
    let result = match args[0].as_str() {
        "--center" => call(OWN_IFACE, "ToggleCenter", |m| m).map(|_| ()),
        "--dnd" => {
            let on = match args.get(1).map(String::as_str) {
                Some("on") => Ok(true),
                Some("off") => Ok(false),
                Some("toggle") | None => call(OWN_IFACE, "GetDoNotDisturb", |m| m)
                    .and_then(|r| r.read1::<bool>().map_err(|e| e.to_string()))
                    .map(|on| !on),
                Some(_) => Err(USAGE.to_string()),
            };
            on.and_then(|on| call(OWN_IFACE, "SetDoNotDisturb", |m| m.append1(on)).map(|_| ()))
        }
        "--send" => {
            let title = args.get(1).cloned().unwrap_or_default();
            let body = args.get(2).cloned().unwrap_or_default();
            notify("odysseus", &title, &body, PropMap::new(), 0)
        }
        "--volume" => volume(),
        "-h" | "--help" => {
            println!("{USAGE}");
            Ok(())
        }
        _ => Err(USAGE.to_string()),
    };
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("odysseus-notify : {e}");
            1
        }
    }
}

fn call(
    iface: &str,
    method: &str,
    build: impl FnOnce(Message) -> Message,
) -> Result<Message, String> {
    let channel = Channel::get_private(BusType::Session).map_err(|e| e.to_string())?;
    let msg = build(Message::new_method_call(NAME, PATH, iface, method)?);
    channel
        .send_with_reply_and_block(msg, Duration::from_secs(10))
        .map_err(|e| e.to_string())
}

fn notify(app: &str, title: &str, body: &str, hints: PropMap, replaces: u32) -> Result<(), String> {
    call(NAME, "Notify", |m| {
        m.append3(app, replaces, "")
            .append3(title, body, Vec::<String>::new())
            .append2(hints, -1i32)
    })
    .map(|_| ())
}

/// Indicateur de volume : lit le volume courant (wpctl) et l'affiche.
fn volume() -> Result<(), String> {
    let out = Command::new("wpctl")
        .args(["get-volume", "@DEFAULT_AUDIO_SINK@"])
        .output()
        .map_err(|e| format!("wpctl : {e}"))?;
    // « Volume: 0.40 » ou « Volume: 0.40 [MUTED] »
    let text = String::from_utf8_lossy(&out.stdout);
    let level = text
        .split_whitespace()
        .nth(1)
        .and_then(|v| v.parse::<f32>().ok())
        .ok_or("volume illisible")?;
    let muted = text.contains("MUTED");
    let value = if muted {
        0
    } else {
        (level * 100.0).round() as i32
    };
    let mut hints = PropMap::new();
    hints.insert("value".into(), Variant(Box::new(value)));
    hints.insert(
        "x-canonical-private-synchronous".into(),
        Variant(Box::new("volume".to_string())),
    );
    let body = if muted {
        "Coupé".to_string()
    } else {
        format!("{value} %")
    };
    notify("Son", "Volume", &body, hints, 0)
}
