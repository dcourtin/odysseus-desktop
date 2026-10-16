//! Découverte par multidiffusion (UDP 224.0.0.167:53317) : odysseus s'annonce
//! en ouvrant la réception, et répond à l'annonce de chaque appareil LocalSend
//! qui arrive (ou rafraîchit sa liste). Sans multidiffusion, LocalSend essaie
//! les adresses du réseau une à une (/register, servi en HTTPS).

use std::{
    net::{Ipv4Addr, UdpSocket},
    os::fd::FromRawFd,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use crate::{Identity, PORT};

const GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 167);

pub fn run(id: &Identity, stop: &AtomicBool) {
    let sock = match bind() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("odysseus-reception : découverte (UDP {PORT}) : {e}");
            return;
        }
    };
    if let Err(e) = sock.join_multicast_v4(&GROUP, &Ipv4Addr::UNSPECIFIED) {
        eprintln!("odysseus-reception : multidiffusion : {e}");
    }
    let _ = sock.set_read_timeout(Some(Duration::from_millis(500)));
    let send = |announce: bool| {
        let msg = id.json(Some(announce)).to_string();
        let _ = sock.send_to(msg.as_bytes(), (GROUP, PORT));
    };
    send(true);
    let mut buf = vec![0u8; 64 * 1024];
    while !stop.load(Ordering::Relaxed) {
        let Ok((n, _from)) = sock.recv_from(&mut buf) else {
            continue;
        };
        let Ok(msg) = serde_json::from_slice::<serde_json::Value>(&buf[..n]) else {
            continue;
        };
        // Notre propre annonce revient aussi
        if msg["fingerprint"].as_str() == Some(id.fingerprint.as_str()) {
            continue;
        }
        if msg["announce"].as_bool() == Some(true) {
            send(false);
        }
    }
}

/// Port partagé (SO_REUSEADDR) : une appli LocalSend ouverte sur cette
/// machine peut écouter les annonces elle aussi
fn bind() -> std::io::Result<UdpSocket> {
    unsafe {
        let fd = libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0);
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let sock = UdpSocket::from_raw_fd(fd);
        let one: libc::c_int = 1;
        for opt in [libc::SO_REUSEADDR, libc::SO_REUSEPORT] {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                opt,
                (&one as *const libc::c_int).cast(),
                size_of::<libc::c_int>() as libc::socklen_t,
            );
        }
        let addr = libc::sockaddr_in {
            sin_family: libc::AF_INET as libc::sa_family_t,
            sin_port: PORT.to_be(),
            sin_addr: libc::in_addr { s_addr: 0 },
            sin_zero: [0; 8],
        };
        if libc::bind(
            fd,
            (&addr as *const libc::sockaddr_in).cast(),
            size_of::<libc::sockaddr_in>() as libc::socklen_t,
        ) < 0
        {
            return Err(std::io::Error::last_os_error());
        }
        Ok(sock)
    }
}
