//! Client de l'IPC du compositeur (protocole i3/sway) : Réglages, odysseus-mcp.

use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    time::Duration,
};

pub const RUN_COMMAND: u32 = 0;
pub const GET_OUTPUTS: u32 = 3;
/// Propre à odysseus : fenêtres de tous les bureaux
pub const GET_WINDOWS: u32 = 100;

/// Socket du compositeur : SWAYSOCK, sinon déduit de WAYLAND_DISPLAY
pub fn socket_path() -> Option<PathBuf> {
    if let Ok(sock) = std::env::var("SWAYSOCK") {
        return Some(sock.into());
    }
    let dir = std::env::var_os("XDG_RUNTIME_DIR")?;
    let display = std::env::var("WAYLAND_DISPLAY").ok()?;
    let display = display.rsplit('/').next().unwrap_or(&display).to_string();
    Some(PathBuf::from(dir).join(format!("odysseus-ipc.{display}.sock")))
}

/// Un message et sa réponse
pub fn request(kind: u32, payload: &str) -> Result<String, String> {
    let path = socket_path().ok_or("compositeur injoignable")?;
    let mut stream = UnixStream::connect(path).map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .map_err(|e| e.to_string())?;
    let mut msg = b"i3-ipc".to_vec();
    msg.extend_from_slice(&(payload.len() as u32).to_ne_bytes());
    msg.extend_from_slice(&kind.to_ne_bytes());
    msg.extend_from_slice(payload.as_bytes());
    stream.write_all(&msg).map_err(|e| e.to_string())?;
    let mut header = [0u8; 14];
    stream.read_exact(&mut header).map_err(|e| e.to_string())?;
    let len = u32::from_ne_bytes([header[6], header[7], header[8], header[9]]) as usize;
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body).map_err(|e| e.to_string())?;
    Ok(String::from_utf8_lossy(&body).into_owned())
}

/// Commande au compositeur (« output scale 1.5 », « nightlight auto »…)
pub fn command(cmd: &str) -> Result<(), String> {
    if request(RUN_COMMAND, cmd)?.contains("\"success\":true") {
        Ok(())
    } else {
        Err(format!("commande refusée : {cmd}"))
    }
}
