//! odysseus-mcp : serveur MCP (Model Context Protocol) d'odysseus-desktop.
//! Un agent (Claude Code, ou tout client MCP) le lance et lui parle en
//! JSON-RPC 2.0 sur l'entrée et la sortie standard, un message par ligne.
//!
//! Il expose le bureau sous forme d'outils, rangés en catégories que
//! l'utilisateur autorise ou non dans Réglages > Agent : lire l'état du
//! système, voir le bureau, voir l'écran, agir, presse-papiers. Chaque appel
//! est consigné (accordé ou refusé) dans ~/.local/state/odysseus/agent.log.
//!
//! Lancé par un agent qui tourne en root, il prend aussitôt l'identité de
//! l'utilisateur du bureau (propriétaire de XDG_RUNTIME_DIR) : il n'agit
//! jamais avec plus de droits que lui.
//!
//! Déclaration dans Claude Code :
//!     claude mcp add odysseus -- ~/odysseus-desktop/target/release/odysseus-mcp

use mcp::tools;

use std::io::{BufRead, Write};

use serde_json::{Value, json};

const PROTOCOLS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

const INSTRUCTIONS: &str = "Serveur du bureau odysseus-desktop (Wayland). \
Il donne l'état de la machine (processeur, mémoire, carte graphique, disques, réseau, \
processus), la liste des fenêtres et des notifications, des captures d'écran, et permet \
d'agir sur le bureau (notifier, ouvrir un fichier, lancer une application, fermer ou déplacer \
une fenêtre, changer de bureau, son, Ne pas déranger, thème, fond d'écran, lumière de nuit, \
échelle) et sur le presse-papiers. Il donne aussi la documentation de la machine (man pages, \
documentation du noyau, notes de construction de cet ordinateur, bibliothèque de \
l'utilisateur), la lecture des fichiers, et l'exécution de commandes que l'utilisateur \
approuve une à une. La mémoire de l'Assistant : les faits que l'utilisateur a accepté de \
retenir (outil memoire) et ses conversations précédentes (conversations) ; quand il confie \
quelque chose de durable sur lui ou sa machine, proposez de le retenir (retenir), il décide \
dans une notification, et jamais un secret. Pour expliquer le système, appuyez-vous sur ces \
sources plutôt que sur votre mémoire, et citez-les. Chaque catégorie est autorisée ou non par \
l'utilisateur dans Réglages > Agent : un refus n'est pas une panne, l'outil « autorisations » \
dit ce qui est permis. Répondez à l'utilisateur en français.";

fn main() {
    drop_privileges();
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else {
            break;
        };
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(Value::Array(batch)) => {
                let replies: Vec<Value> = batch.iter().filter_map(handle).collect();
                (!replies.is_empty()).then_some(Value::Array(replies))
            }
            Ok(msg) => handle(&msg),
            Err(e) => Some(error(Value::Null, -32700, &format!("JSON illisible : {e}"))),
        };
        if let Some(reply) = reply {
            let _ = writeln!(stdout, "{reply}");
            let _ = stdout.flush();
        }
    }
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

/// Une requête (avec identifiant) reçoit une réponse ; une notification, rien.
fn handle(msg: &Value) -> Option<Value> {
    let id = msg.get("id")?.clone();
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => {
            let asked = params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or(PROTOCOLS[0]);
            let version = PROTOCOLS
                .iter()
                .find(|v| **v == asked)
                .unwrap_or(&PROTOCOLS[0]);
            json!({
                "protocolVersion": version,
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {
                    "name": "odysseus-mcp",
                    "title": "odysseus-desktop",
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "instructions": INSTRUCTIONS,
            })
        }
        "ping" => json!({}),
        "tools/list" => json!({"tools": tools::list()}),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            tools::call(name, &args)
        }
        "resources/list" => json!({"resources": []}),
        "prompts/list" => json!({"prompts": []}),
        _ => return Some(error(id, -32601, &format!("méthode inconnue : {method}"))),
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

/// Lancé en root (l'agent tourne en root) : on devient l'utilisateur du
/// bureau, propriétaire de XDG_RUNTIME_DIR, avec son environnement de session.
fn drop_privileges() {
    use std::os::unix::fs::MetadataExt;
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            // Sans XDG_RUNTIME_DIR : la session graphique de l'uid 1000 si elle existe
            let p = std::path::PathBuf::from("/run/user/1000");
            p.exists().then_some(p)
        });
    if let Some(dir) = runtime.as_ref() {
        // Ces variables sont posées avant tout fil : le processus est encore seul
        unsafe {
            std::env::set_var("XDG_RUNTIME_DIR", dir);
            if std::env::var_os("WAYLAND_DISPLAY").is_none()
                && let Some(display) = newest_wayland_socket(dir)
            {
                std::env::set_var("WAYLAND_DISPLAY", display);
            }
        }
    }
    if unsafe { libc::geteuid() } != 0 {
        return;
    }
    let Some(uid) = runtime
        .as_ref()
        .and_then(|d| std::fs::metadata(d).ok())
        .map(|m| m.uid())
        .filter(|uid| *uid != 0)
    else {
        return;
    };
    let pw = unsafe { libc::getpwuid(uid) };
    if pw.is_null() {
        return;
    }
    let (name, home, gid) = unsafe {
        (
            std::ffi::CStr::from_ptr((*pw).pw_name).to_owned(),
            std::ffi::CStr::from_ptr((*pw).pw_dir)
                .to_string_lossy()
                .into_owned(),
            (*pw).pw_gid,
        )
    };
    let ok = unsafe {
        libc::initgroups(name.as_ptr(), gid) == 0
            && libc::setgid(gid) == 0
            && libc::setuid(uid) == 0
    };
    if !ok {
        eprintln!("odysseus-mcp : impossible de prendre l'identité de l'utilisateur, arrêt");
        std::process::exit(1);
    }
    let user = name.to_string_lossy().into_owned();
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("USER", &user);
        std::env::set_var("LOGNAME", &user);
        std::env::remove_var("XDG_CONFIG_HOME");
        std::env::remove_var("XDG_DATA_HOME");
        if let Some(dir) = runtime {
            std::env::set_var(
                "DBUS_SESSION_BUS_ADDRESS",
                format!("unix:path={}", dir.join("bus").display()),
            );
        }
    }
}

/// Bureau Wayland le plus récent (wayland-1, wayland-2…)
fn newest_wayland_socket(dir: &std::path::Path) -> Option<String> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| {
            let n = e.file_name();
            let n = n.to_string_lossy();
            n.starts_with("wayland-") && !n.ends_with(".lock")
        })
        .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
}
