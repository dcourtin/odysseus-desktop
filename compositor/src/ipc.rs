//! Sous-ensemble de l'IPC de sway (protocole i3) : la même barre — ou des
//! outils comme waybar — fonctionnent sous sway et sous odysseus.
//! Socket : $XDG_RUNTIME_DIR/odysseus-ipc.<WAYLAND_DISPLAY>.sock, transmis aux
//! clients dans SWAYSOCK.
//!
//! Messages pris en charge : RUN_COMMAND (« workspace N », « workspace number N »,
//! « workspace next_on_output|prev_on_output »), GET_WORKSPACES, SUBSCRIBE
//! (événements « workspace »).

use std::{
    ffi::OsStr,
    io::{self, Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::PathBuf,
};

use smithay::reexports::calloop::{Interest, LoopHandle, Mode, PostAction, generic::Generic};

use crate::{CalloopData, Odysseus};

const MAGIC: &[u8; 6] = b"i3-ipc";
const RUN_COMMAND: u32 = 0;
const GET_WORKSPACES: u32 = 1;
const SUBSCRIBE: u32 = 2;
/// Bit des événements ; l'événement « workspace » a le type 0
const EVENT_WORKSPACE: u32 = 0x8000_0000;

fn message(kind: u32, payload: &str) -> Vec<u8> {
    let mut msg = Vec::with_capacity(14 + payload.len());
    msg.extend_from_slice(MAGIC);
    msg.extend_from_slice(&(payload.len() as u32).to_ne_bytes());
    msg.extend_from_slice(&kind.to_ne_bytes());
    msg.extend_from_slice(payload.as_bytes());
    msg
}

pub fn init(
    handle: &LoopHandle<'static, CalloopData>,
    wayland_socket: &OsStr,
) -> io::Result<PathBuf> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .ok_or_else(|| io::Error::other("XDG_RUNTIME_DIR absent"))?;
    let path = PathBuf::from(dir).join(format!(
        "odysseus-ipc.{}.sock",
        wayland_socket.to_string_lossy()
    ));
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path)?;
    listener.set_nonblocking(true)?;

    let loop_handle = handle.clone();
    handle
        .insert_source(
            Generic::new(listener, Interest::READ, Mode::Level),
            move |_, listener, _| {
                loop {
                    match listener.as_ref().accept() {
                        Ok((stream, _)) => {
                            if let Err(e) = add_client(&loop_handle, stream) {
                                tracing::warn!("IPC : client refusé ({e})");
                            }
                        }
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                        Err(e) => {
                            tracing::warn!("IPC : accept ({e})");
                            break;
                        }
                    }
                }
                Ok(PostAction::Continue)
            },
        )
        .map_err(|e| io::Error::other(e.to_string()))?;
    Ok(path)
}

fn add_client(handle: &LoopHandle<'static, CalloopData>, stream: UnixStream) -> io::Result<()> {
    stream.set_nonblocking(true)?;
    let mut buffer: Vec<u8> = Vec::new();
    handle
        .insert_source(
            Generic::new(stream, Interest::READ, Mode::Level),
            move |_, stream, data| {
                let mut s: &UnixStream = stream.as_ref();
                let mut chunk = [0u8; 4096];
                loop {
                    match s.read(&mut chunk) {
                        Ok(0) => return Ok(PostAction::Remove),
                        Ok(n) => buffer.extend_from_slice(&chunk[..n]),
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                        Err(_) => return Ok(PostAction::Remove),
                    }
                }
                // Messages complets : en-tête de 14 octets + charge utile
                while buffer.len() >= 14 {
                    if &buffer[..6] != MAGIC {
                        return Ok(PostAction::Remove);
                    }
                    let len =
                        u32::from_ne_bytes([buffer[6], buffer[7], buffer[8], buffer[9]]) as usize;
                    let kind = u32::from_ne_bytes([buffer[10], buffer[11], buffer[12], buffer[13]]);
                    if buffer.len() < 14 + len {
                        break;
                    }
                    let payload = String::from_utf8_lossy(&buffer[14..14 + len]).into_owned();
                    buffer.drain(..14 + len);
                    let reply = handle_message(&mut data.state, kind, &payload, s);
                    if s.write_all(&message(kind, &reply)).is_err() {
                        return Ok(PostAction::Remove);
                    }
                }
                Ok(PostAction::Continue)
            },
        )
        .map_err(|e| io::Error::other(e.to_string()))?;
    Ok(())
}

fn handle_message(state: &mut Odysseus, kind: u32, payload: &str, stream: &UnixStream) -> String {
    match kind {
        RUN_COMMAND => {
            let ok = run_command(state, payload.trim());
            format!("[{{\"success\":{ok}}}]")
        }
        GET_WORKSPACES => workspaces_json(state),
        SUBSCRIBE => {
            if payload.contains("\"workspace\"")
                && let Ok(clone) = stream.try_clone()
            {
                state.ipc_subscribers.push(clone);
            }
            "{\"success\":true}".into()
        }
        _ => "{\"success\":false,\"error\":\"message non pris en charge\"}".into(),
    }
}

fn run_command(state: &mut Odysseus, command: &str) -> bool {
    let Some(arg) = command.strip_prefix("workspace") else {
        return false;
    };
    let arg = arg.trim();
    let arg = arg
        .strip_prefix("number")
        .unwrap_or(arg)
        .trim()
        .trim_matches('"');
    match arg {
        "next_on_output" | "next" => state.cycle_workspace(1),
        "prev_on_output" | "prev" => state.cycle_workspace(-1),
        n => match n.parse::<usize>() {
            Ok(num) if (1..=crate::workspaces::COUNT).contains(&num) => {
                state.switch_workspace(num - 1)
            }
            _ => return false,
        },
    }
    true
}

fn workspaces_json(state: &Odysseus) -> String {
    let output = state
        .space
        .outputs()
        .next()
        .map(|o| o.name())
        .unwrap_or_default();
    let items: Vec<String> = state
        .workspace_list()
        .iter()
        .map(|w| {
            format!(
                "{{\"num\":{n},\"name\":\"{n}\",\"focused\":{f},\"visible\":{f},\"urgent\":false,\"output\":\"{output}\"}}",
                n = w.num,
                f = w.focused
            )
        })
        .collect();
    format!("[{}]", items.join(","))
}

/// Prévient les abonnés qu'un bureau a changé ; ils relisent la liste.
pub fn broadcast_workspace_event(state: &mut Odysseus) {
    if state.ipc_subscribers.is_empty() {
        return;
    }
    let msg = message(EVENT_WORKSPACE, "{\"change\":\"focus\"}");
    state.ipc_subscribers.retain(|s| {
        let mut s: &UnixStream = s;
        s.write_all(&msg).is_ok()
    });
}
