//! Demandes de mot de passe du trousseau (oo7-daemon) : interface
//! org.freedesktop.secrets.CliPrompter. Une session odysseus est de type
//! « tty » pour logind : oo7 s'adresse alors à cette interface.
//!
//! À chaque demande, une paire de sockets : le démon reçoit tout de suite
//! l'extrémité de lecture ; la fenêtre odysseus-trousseau reçoit l'autre
//! comme sortie standard et y écrit le mot de passe saisi (rien si
//! l'utilisateur annule). Le mot de passe ne passe jamais par ce processus.

use std::{
    os::{fd::IntoRawFd, unix::net::UnixStream},
    process::{Command, Stdio},
    time::Duration,
};

use dbus::{
    Message,
    channel::{BusType, Channel},
};

const NAME: &str = "org.freedesktop.secrets.CliPrompter";
const INTROSPECTION: &str = r#"<!DOCTYPE node PUBLIC "-//freedesktop//DTD D-BUS Object Introspection 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/introspect.dtd">
<node>
 <interface name="org.freedesktop.secrets.CliPrompter">
  <method name="Prompt"><arg type="s" name="label" direction="in"/><arg type="s" name="description" direction="in"/><arg type="h" direction="out"/><arg type="b" direction="out"/></method>
 </interface>
 <interface name="org.freedesktop.DBus.Introspectable">
  <method name="Introspect"><arg direction="out" type="s"/></method>
 </interface>
</node>"#;

pub struct Prompter {
    channel: Channel,
}

/// Ce que demande oo7, d'après sa description (en anglais, sans traduction française)
fn role(description: &str) -> &'static str {
    let d = description.to_lowercase();
    if d.contains("create a new keyring") {
        "creer"
    } else if d.contains("change the password") {
        "changer"
    } else {
        "ouvrir"
    }
}

impl Prompter {
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

    pub fn fd(&self) -> std::os::fd::RawFd {
        self.channel.watch().fd
    }

    fn reply(&self, msg: Message) {
        let _ = self.channel.send(msg);
        self.channel.flush();
    }

    pub fn poll(&mut self) {
        let _ = self.channel.read_write(Some(Duration::ZERO));
        while let Some(msg) = self.channel.pop_message() {
            if msg.msg_type() != dbus::MessageType::MethodCall {
                continue;
            }
            match msg.member().as_deref() {
                Some("Prompt") => {
                    let (label, description): (Option<String>, Option<String>) = msg.get2();
                    match spawn_window(
                        label.as_deref().unwrap_or("login"),
                        role(description.as_deref().unwrap_or("")),
                    ) {
                        Ok(read) => {
                            let fd = unsafe { dbus::arg::OwnedFd::new(read.into_raw_fd()) };
                            self.reply(msg.method_return().append2(fd, true));
                        }
                        Err(e) => {
                            eprintln!("odysseus-notify : fenêtre du trousseau ({e})");
                            self.reply(msg.error(
                                &"org.freedesktop.DBus.Error.Failed".into(),
                                c"fenêtre du trousseau indisponible",
                            ));
                        }
                    }
                }
                Some("Introspect") => self.reply(msg.method_return().append1(INTROSPECTION)),
                Some("Ping") => self.reply(msg.method_return()),
                _ => self.reply(msg.error(
                    &"org.freedesktop.DBus.Error.UnknownMethod".into(),
                    c"méthode inconnue",
                )),
            }
        }
    }
}

/// Lance la fenêtre ; renvoie l'extrémité de lecture, pour oo7
fn spawn_window(label: &str, role: &str) -> std::io::Result<UnixStream> {
    let (read, write) = UnixStream::pair()?;
    // Après une mise à jour, /proc/self/exe se termine par « (deleted) » : même dossier
    let exe = std::env::current_exe()
        .ok()
        .map(|p| std::path::PathBuf::from(p.to_string_lossy().trim_end_matches(" (deleted)")))
        .and_then(|p| p.parent().map(|d| d.join("odysseus-trousseau")))
        .unwrap_or_else(|| "odysseus-trousseau".into());
    let mut child = Command::new(exe)
        .args(["--demander", role, label])
        .stdin(Stdio::null())
        .stdout(Stdio::from(std::os::fd::OwnedFd::from(write)))
        .spawn()?;
    // Récupéré à la fin pour ne pas laisser de zombie
    std::thread::spawn(move || child.wait());
    Ok(read)
}
