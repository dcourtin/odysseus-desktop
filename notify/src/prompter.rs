//! Demandes de mot de passe du trousseau (oo7-daemon) : interface
//! org.freedesktop.secrets.CliPrompter. Une session odysseus est de type
//! « tty » pour logind : oo7 s'adresse alors à cette interface.
//!
//! À chaque demande, une paire de sockets : la fenêtre odysseus-trousseau
//! reçoit l'extrémité d'écriture comme sortie standard et y écrit le mot de
//! passe saisi. La réponse à oo7 (extrémité de lecture, et « oui » ou « non »)
//! attend que la fenêtre se ferme : « non » quand l'utilisateur annule, pour
//! que l'application reçoive un refus au lieu d'attendre sans fin. Le mot de
//! passe ne passe jamais par ce processus.
//!
//! oo7 (0.7) ne dit rien à l'application quand le mot de passe est faux : il
//! ne redemande pas et n'émet pas le signal « Completed » qu'elle attend. On
//! guette donc ce signal ; s'il ne vient pas et que le trousseau est toujours
//! verrouillé, on relance la demande (méthode Prompt de l'objet d'oo7) avec
//! « Mot de passe incorrect » dans la fenêtre.

use std::{
    os::{fd::IntoRawFd, unix::net::UnixStream},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use dbus::{
    Message,
    arg::Variant,
    channel::{BusType, Channel},
};

const NAME: &str = "org.freedesktop.secrets.CliPrompter";
const SECRETS: &str = "org.freedesktop.secrets";
/// Signal que l'application attend ; oo7 l'émet seulement si le mot de passe est bon
const COMPLETED_RULE: &str = "type='signal',interface='org.freedesktop.Secret.Prompt',member='Completed'";
/// Délai laissé à oo7 pour vérifier le mot de passe et signaler « Completed »
const VERDICT: Duration = Duration::from_secs(4);
const RETRY_HINT: &str = "Mot de passe incorrect, réessayez";
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
    /// Fenêtres ouvertes : la réponse à oo7 attend la décision de l'utilisateur
    pending: Vec<Pending>,
    /// Mots de passe transmis, en attente du verdict d'oo7
    awaiting: Vec<Awaiting>,
    /// Trousseau dont la prochaine demande est une nouvelle tentative
    retry: Option<String>,
}

struct Pending {
    call: Message,
    child: Child,
    read: UnixStream,
    label: String,
    /// Objet Prompt d'oo7 (pour relancer la demande)
    prompt_path: Option<String>,
}

struct Awaiting {
    label: String,
    prompt_path: Option<String>,
    deadline: Instant,
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

/// Chemin de la collection d'oo7 pour un trousseau, d'après son nom (même
/// règle qu'oo7 : minuscules, tout sauf lettres, chiffres et « _ » devient « _ »)
fn collection_path(label: &str) -> String {
    let name: String = label
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
        .collect();
    format!("/org/freedesktop/secrets/collection/{name}")
}

/// Le dernier objet Prompt d'oo7 (pN, N croissant), d'après l'introspection de
/// /org/freedesktop/secrets/prompt
fn latest_prompt_node(xml: &str) -> Option<String> {
    xml.split("<node name=\"")
        .skip(1)
        .filter_map(|rest| rest.split('"').next())
        .filter_map(|name| name.strip_prefix('p').and_then(|n| n.parse::<u64>().ok()))
        .max()
        .map(|n| format!("/org/freedesktop/secrets/prompt/p{n}"))
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
        if !matches!(reply.read1::<u32>(), Ok(1) | Ok(4)) {
            return None;
        }
        let add = Message::new_method_call(
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "AddMatch",
        )
        .ok()?
        .append1(COMPLETED_RULE);
        let _ = channel.send_with_reply_and_block(add, Duration::from_secs(5));
        Some(Self {
            channel,
            pending: Vec::new(),
            awaiting: Vec::new(),
            retry: None,
        })
    }

    pub fn fd(&self) -> std::os::fd::RawFd {
        self.channel.watch().fd
    }

    fn reply(&self, msg: Message) {
        let _ = self.channel.send(msg);
        self.channel.flush();
    }

    /// Appel à oo7 avec réponse (il reste réactif pendant qu'il attend la nôtre)
    fn ask(&self, path: &str, interface: &str, member: &str) -> Option<Message> {
        let msg = Message::new_method_call(SECRETS, path, interface, member).ok()?;
        self.channel
            .send_with_reply_and_block(msg, Duration::from_secs(2))
            .ok()
    }

    fn latest_prompt(&self) -> Option<String> {
        let reply = self.ask(
            "/org/freedesktop/secrets/prompt",
            "org.freedesktop.DBus.Introspectable",
            "Introspect",
        )?;
        latest_prompt_node(&reply.read1::<String>().ok()?)
    }

    /// Le trousseau est-il verrouillé ? None : oo7 n'a pas répondu
    fn locked(&self, label: &str) -> Option<bool> {
        let msg = Message::new_method_call(
            SECRETS,
            collection_path(label),
            "org.freedesktop.DBus.Properties",
            "Get",
        )
        .ok()?
        .append2("org.freedesktop.Secret.Collection", "Locked");
        let reply = self
            .channel
            .send_with_reply_and_block(msg, Duration::from_secs(2))
            .ok()?;
        reply.read1::<Variant<bool>>().ok().map(|v| v.0)
    }

    pub fn poll(&mut self) {
        let _ = self.channel.read_write(Some(Duration::ZERO));
        while let Some(msg) = self.channel.pop_message() {
            match msg.msg_type() {
                dbus::MessageType::MethodCall => self.method_call(msg),
                dbus::MessageType::Signal => self.signal(&msg),
                // Réponses à nos propres appels (relance d'une demande) : rien à en faire
                _ => {}
            }
        }
    }

    fn method_call(&mut self, msg: Message) {
        match msg.member().as_deref() {
            Some("Prompt") => {
                let (label, description): (Option<String>, Option<String>) = msg.get2();
                let label = label.unwrap_or_else(|| "login".into());
                let hint = (self.retry.take_if(|l| *l == label)).map(|_| RETRY_HINT);
                let prompt_path = self.latest_prompt();
                match spawn_window(&label, role(description.as_deref().unwrap_or("")), hint) {
                    Ok((child, read)) => self.pending.push(Pending {
                        call: msg,
                        child,
                        read,
                        label,
                        prompt_path,
                    }),
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

    /// « Completed » : oo7 a accepté le mot de passe (ou l'application a renoncé)
    fn signal(&mut self, msg: &Message) {
        if msg.member().as_deref() != Some("Completed") {
            return;
        }
        let path = msg.path().map(|p| p.to_string());
        match self.awaiting.iter().position(|a| a.prompt_path == path) {
            Some(i) => {
                self.awaiting.remove(i);
            }
            // Objet inconnu : le plus ancien en attente
            None if !self.awaiting.is_empty() => {
                self.awaiting.remove(0);
            }
            None => {}
        }
    }

    /// À appeler régulièrement : fenêtres fermées, verdicts en retard
    pub fn tick(&mut self) {
        let mut i = 0;
        while i < self.pending.len() {
            match self.pending[i].child.try_wait() {
                Ok(Some(status)) => {
                    let p = self.pending.remove(i);
                    let fd = unsafe { dbus::arg::OwnedFd::new(p.read.into_raw_fd()) };
                    // Mot de passe écrit (code 0) ou annulé : oo7 répond alors
                    // un refus à l'application au lieu de la laisser attendre
                    let given = status.success();
                    self.reply(p.call.method_return().append2(fd, given));
                    if given {
                        self.awaiting.push(Awaiting {
                            label: p.label,
                            prompt_path: p.prompt_path,
                            deadline: Instant::now() + VERDICT,
                        });
                    }
                }
                Ok(None) => i += 1,
                Err(_) => {
                    let p = self.pending.remove(i);
                    let fd = unsafe { dbus::arg::OwnedFd::new(p.read.into_raw_fd()) };
                    self.reply(p.call.method_return().append2(fd, false));
                }
            }
        }
        if self.awaiting.is_empty() {
            return;
        }
        let now = Instant::now();
        let late: Vec<Awaiting> = {
            let (late, rest): (Vec<_>, Vec<_>) =
                std::mem::take(&mut self.awaiting).into_iter().partition(|a| a.deadline <= now);
            self.awaiting = rest;
            late
        };
        for a in late {
            // Pas de « Completed » : si le trousseau est resté verrouillé, le mot
            // de passe était faux et oo7 n'en dira rien à l'application
            if self.locked(&a.label) != Some(true) {
                continue;
            }
            match a.prompt_path {
                Some(path) => {
                    eprintln!("odysseus-notify : mot de passe du trousseau « {} » refusé, nouvelle demande", a.label);
                    self.retry = Some(a.label);
                    if let Ok(msg) =
                        Message::new_method_call(SECRETS, path, "org.freedesktop.Secret.Prompt", "Prompt")
                    {
                        // Sans attendre la réponse : elle viendra après la fenêtre
                        let _ = self.channel.send(msg.append1(""));
                        self.channel.flush();
                    }
                }
                None => eprintln!(
                    "odysseus-notify : mot de passe du trousseau « {} » refusé, et pas de demande à relancer",
                    a.label
                ),
            }
        }
    }
}

/// Lance la fenêtre ; renvoie le processus et l'extrémité de lecture, pour oo7
fn spawn_window(
    label: &str,
    role: &str,
    hint: Option<&str>,
) -> std::io::Result<(Child, UnixStream)> {
    let (read, write) = UnixStream::pair()?;
    // Après une mise à jour, /proc/self/exe se termine par « (deleted) » : même dossier
    let exe = std::env::current_exe()
        .ok()
        .map(|p| std::path::PathBuf::from(p.to_string_lossy().trim_end_matches(" (deleted)")))
        .and_then(|p| p.parent().map(|d| d.join("odysseus-trousseau")))
        .unwrap_or_else(|| "odysseus-trousseau".into());
    let mut cmd = Command::new(exe);
    cmd.args(["--demander", role, label]);
    if let Some(hint) = hint {
        cmd.args(["--erreur", hint]);
    }
    let child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::from(std::os::fd::OwnedFd::from(write)))
        .spawn()?;
    Ok((child, read))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles() {
        assert_eq!(role("An application wants to create a new keyring called “x”."), "creer");
        assert_eq!(role("An application wants to change the password for the “x” keyring."), "changer");
        assert_eq!(role("An application wants access to the keyring “Login”, but it is locked"), "ouvrir");
    }

    #[test]
    fn collection_paths() {
        assert_eq!(collection_path("Login"), "/org/freedesktop/secrets/collection/login");
        assert_eq!(collection_path("Mes secrets !"), "/org/freedesktop/secrets/collection/mes_secrets__");
    }

    #[test]
    fn latest_prompt() {
        let xml = "<node>\n <node name=\"p3\"/>\n <node name=\"p12\"/>\n <node name=\"autre\"/>\n</node>";
        assert_eq!(latest_prompt_node(xml).as_deref(), Some("/org/freedesktop/secrets/prompt/p12"));
        assert_eq!(latest_prompt_node("<node></node>"), None);
    }
}
