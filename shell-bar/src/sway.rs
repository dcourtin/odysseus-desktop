//! Client minimal de l'IPC sway (protocole i3-ipc) : en-tête « i3-ipc »,
//! longueur et type en u32 natifs, puis charge utile JSON.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;

use serde::Deserialize;
use smithay_client_toolkit::reexports::calloop::channel::Sender;

const MAGIC: &[u8; 6] = b"i3-ipc";
const RUN_COMMAND: u32 = 0;
const GET_WORKSPACES: u32 = 1;
const SUBSCRIBE: u32 = 2;
/// Propre à odysseus : partages d'écran en cours
const GET_SCREENCAST: u32 = 101;

#[derive(Deserialize, Clone, Debug)]
pub struct Workspace {
    pub num: i32,
    pub name: String,
    pub focused: bool,
    pub urgent: bool,
}

pub struct Ipc {
    stream: UnixStream,
}

impl Ipc {
    pub fn connect() -> io::Result<Self> {
        let path = std::env::var_os("SWAYSOCK").ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "SWAYSOCK absent : pas sous sway ?")
        })?;
        Ok(Self {
            stream: UnixStream::connect(path)?,
        })
    }

    fn send(&mut self, kind: u32, payload: &str) -> io::Result<()> {
        let mut msg = Vec::with_capacity(14 + payload.len());
        msg.extend_from_slice(MAGIC);
        msg.extend_from_slice(&(payload.len() as u32).to_ne_bytes());
        msg.extend_from_slice(&kind.to_ne_bytes());
        msg.extend_from_slice(payload.as_bytes());
        self.stream.write_all(&msg)
    }

    fn recv(&mut self) -> io::Result<Vec<u8>> {
        let mut header = [0u8; 14];
        self.stream.read_exact(&mut header)?;
        if &header[..6] != MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "réponse IPC sway invalide",
            ));
        }
        let len = u32::from_ne_bytes([header[6], header[7], header[8], header[9]]) as usize;
        let mut body = vec![0; len];
        self.stream.read_exact(&mut body)?;
        Ok(body)
    }

    /// Bureaux triés par numéro.
    pub fn workspaces(&mut self) -> io::Result<Vec<Workspace>> {
        self.send(GET_WORKSPACES, "")?;
        let mut list: Vec<Workspace> =
            serde_json::from_slice(&self.recv()?).map_err(io::Error::other)?;
        list.sort_by_key(|w| w.num);
        Ok(list)
    }

    /// Partages d'écran en cours (0 sous sway, qui ne connaît pas ce message)
    pub fn screencast(&mut self) -> io::Result<u64> {
        self.send(GET_SCREENCAST, "")?;
        let v: serde_json::Value =
            serde_json::from_slice(&self.recv()?).map_err(io::Error::other)?;
        Ok(v["sessions"].as_u64().unwrap_or(0))
    }

    pub fn command(&mut self, cmd: &str) -> io::Result<()> {
        self.send(RUN_COMMAND, cmd)?;
        self.recv().map(drop)
    }
}

/// Bloquant, à lancer dans un fil : prévient la boucle principale à chaque
/// événement « workspace » de sway.
pub fn watch(tx: Sender<()>) -> io::Result<()> {
    let mut ipc = Ipc::connect()?;
    ipc.send(SUBSCRIBE, r#"["workspace"]"#)?;
    ipc.recv()?;
    loop {
        ipc.recv()?;
        if tx.send(()).is_err() {
            return Ok(());
        }
    }
}
