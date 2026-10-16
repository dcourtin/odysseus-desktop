//! odysseus-reception : fichiers reçus des autres appareils du réseau local.
//!
//! Protocole ouvert de LocalSend (appli gratuite pour macOS, iOS, Android,
//! Windows, Linux) : odysseus apparaît dans la liste des appareils de
//! LocalSend ; chaque envoi est accepté ou refusé dans une notification, et
//! les fichiers arrivent dans Téléchargements. Un message texte s'affiche dans
//! une notification (bouton « Copier »).
//!
//! Démon lancé avec la session. Il ne s'ouvre au réseau que lorsque Réglages >
//! Réception l'autorise (clé `reception` du fichier de réglages) et suit ce
//! réglage à chaud. Seules les adresses du réseau local sont servies.

mod ask;
mod discovery;
mod http;
mod receive;
mod tls;

use std::{
    io::{BufReader, Write},
    net::{IpAddr, SocketAddr, TcpListener, TcpStream},
    os::fd::{AsFd, AsRawFd},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use rustls::{ServerConfig, ServerConnection, StreamOwned};

/// Port de LocalSend (TCP pour HTTPS, UDP pour la découverte)
pub const PORT: u16 = 53317;

/// Ce que les autres appareils voient d'odysseus
pub struct Identity {
    pub alias: String,
    pub fingerprint: String,
}

impl Identity {
    /// Fiche de l'appareil (annonce, /register, /info)
    pub fn json(&self, announce: Option<bool>) -> serde_json::Value {
        let mut v = serde_json::json!({
            "alias": self.alias,
            "version": "2.1",
            "deviceModel": "odysseus",
            "deviceType": "desktop",
            "fingerprint": self.fingerprint,
            "port": PORT,
            "protocol": "https",
            "download": false,
        });
        if let Some(a) = announce {
            v["announce"] = a.into();
        }
        v
    }
}

fn main() {
    let tls = match tls::load_or_create() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("odysseus-reception : certificat : {e}");
            std::process::exit(1);
        }
    };
    let id = Arc::new(Identity {
        alias: alias(),
        fingerprint: tls.fingerprint.clone(),
    });
    let watch = common::settings::path()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .and_then(|d| common::watch::dir(&d));
    let mut running: Option<Arc<AtomicBool>> = None;
    loop {
        let wanted = common::settings::Settings::load().reception;
        match (&running, wanted) {
            (None, true) => match start(tls.config.clone(), id.clone()) {
                Ok(stop) => {
                    eprintln!("réception ouverte : « {} », port {PORT}", id.alias);
                    running = Some(stop);
                }
                Err(e) => eprintln!("odysseus-reception : port {PORT} : {e}"),
            },
            (Some(stop), false) => {
                stop.store(true, Ordering::Relaxed);
                running = None;
                eprintln!("réception fermée");
            }
            _ => {}
        }
        // Prochain réveil : une modification des réglages ; si l'ouverture a
        // échoué (port pris), un nouvel essai dans 30 s
        let timeout = if wanted && running.is_none() {
            30_000
        } else {
            -1
        };
        match &watch {
            Some(fd) => {
                let mut pfd = libc::pollfd {
                    fd: fd.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                unsafe { libc::poll(&mut pfd, 1, timeout) };
                common::watch::drain(fd.as_fd());
                // Le fichier est écrit puis renommé : laisser l'opération finir
                thread::sleep(Duration::from_millis(100));
            }
            None => thread::sleep(Duration::from_secs(5)),
        }
    }
}

/// Nom de l'appareil : celui de la machine
fn alias() -> String {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "odysseus".into())
}

/// Ouvre le port et lance la découverte ; renvoie le drapeau qui les arrête
fn start(config: Arc<ServerConfig>, id: Arc<Identity>) -> std::io::Result<Arc<AtomicBool>> {
    let listener = TcpListener::bind(("0.0.0.0", PORT))?;
    listener.set_nonblocking(true)?;
    let stop = Arc::new(AtomicBool::new(false));
    let receiver = Arc::new(receive::Receiver::new());
    {
        let (stop, id) = (stop.clone(), id.clone());
        thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((tcp, peer)) if local(peer.ip()) => {
                        let (config, id, receiver) = (config.clone(), id.clone(), receiver.clone());
                        thread::spawn(move || serve(tcp, peer, config, &id, &receiver));
                    }
                    Ok(_) => {}
                    Err(_) => thread::sleep(Duration::from_millis(200)),
                }
            }
        });
    }
    {
        let stop = stop.clone();
        thread::spawn(move || discovery::run(&id, &stop));
    }
    Ok(stop)
}

/// Adresse du réseau local (privée, lien local ou cette machine)
fn local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_link_local() || v4.is_loopback(),
        IpAddr::V6(v6) => v6.to_ipv4_mapped().is_some_and(|v4| local(IpAddr::V4(v4))),
    }
}

/// Une connexion : TLS, puis des requêtes HTTP à la suite
fn serve(
    tcp: TcpStream,
    peer: SocketAddr,
    config: Arc<ServerConfig>,
    id: &Identity,
    receiver: &receive::Receiver,
) {
    let _ = tcp.set_nonblocking(false);
    let _ = tcp.set_read_timeout(Some(Duration::from_secs(60)));
    let _ = tcp.set_write_timeout(Some(Duration::from_secs(60)));
    let Ok(conn) = ServerConnection::new(config) else {
        return;
    };
    let mut stream = BufReader::with_capacity(256 * 1024, StreamOwned::new(conn, tcp));
    loop {
        let req = match http::read_request(&mut stream) {
            Ok(Some(req)) => req,
            _ => break,
        };
        let keep = match receiver.handle(&req, &mut stream, peer.ip(), id) {
            Ok(keep) => keep,
            Err(_) => false,
        };
        if stream.get_mut().flush().is_err()
            || !keep
            || req.header("connection").is_some_and(|c| c.eq_ignore_ascii_case("close"))
        {
            break;
        }
    }
    let inner = stream.get_mut();
    inner.conn.send_close_notify();
    let _ = inner.flush();
}
