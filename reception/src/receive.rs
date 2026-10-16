//! Envois reçus : une session à la fois. L'appareil annonce ses fichiers
//! (prepare-upload), l'utilisateur accepte ou refuse dans une notification,
//! puis chaque fichier arrive par sa propre requête (upload), écrit à côté
//! sous un nom provisoire et renommé une fois complet (et vérifié, si
//! l'empreinte SHA-256 a été annoncée).

use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{self, BufReader, Read, Write},
    net::IpAddr,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Mutex,
    thread,
    time::{Duration, Instant},
};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{
    Identity, ask,
    http::{self, Request},
};

/// Sans réponse dans ce délai, l'envoi est refusé
const ASK_TIMEOUT: Duration = Duration::from_secs(90);
/// Session abandonnée par l'appareil (plus rien reçu depuis)
const IDLE: Duration = Duration::from_secs(300);
/// Corps JSON (liste des fichiers, aperçus compris)
const JSON_LIMIT: usize = 64 * 1024 * 1024;
/// Jauge de progression au-delà de cette taille
const PROGRESS_MIN: u64 = 20_000_000;
/// Fichiers nommés dans la demande d'accord
const LISTED: usize = 5;

#[derive(PartialEq, Clone, Copy)]
enum SlotState {
    Waiting,
    Writing,
    Done,
    Failed,
}

struct Slot {
    token: String,
    dest: PathBuf,
    sha256: Option<String>,
    state: SlotState,
}

struct Session {
    id: String,
    peer: IpAddr,
    alias: String,
    slots: HashMap<String, Slot>,
    total: u64,
    received: u64,
    last: Instant,
    progress_at: Option<Instant>,
    /// Notification de progression (remplacée par celle du résultat)
    notif: u32,
    saved: Vec<PathBuf>,
}

#[derive(Default)]
struct State {
    session: Option<Session>,
    /// Une demande d'accord est affichée
    asking: bool,
}

pub struct Receiver {
    state: Mutex<State>,
}

/// Fichier proposé par l'appareil
struct Offer {
    id: String,
    name: String,
    size: u64,
    sha256: Option<String>,
}

impl Receiver {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(State::default()),
        }
    }

    /// Traite une requête ; false : fermer la connexion
    pub fn handle<S: Read + Write>(
        &self,
        req: &Request,
        s: &mut BufReader<S>,
        peer: IpAddr,
        id: &Identity,
    ) -> io::Result<bool> {
        let path = req.path.trim_end_matches('/');
        match (req.method.as_str(), path) {
            (_, "/api/localsend/v2/info" | "/api/localsend/v1/info")
            | ("POST", "/api/localsend/v2/register" | "/api/localsend/v1/register") => {
                req.body(s).skip(JSON_LIMIT as u64)?;
                http::respond(s.get_mut(), 200, Some(&id.json(None)))?;
                Ok(true)
            }
            ("POST", "/api/localsend/v2/prepare-upload") => {
                let body = req.body(s).read_all(JSON_LIMIT)?;
                let (status, json) = match serde_json::from_slice::<Value>(&body) {
                    Ok(v) => self.prepare(&v, peer),
                    Err(_) => (400, None),
                };
                http::respond(s.get_mut(), status, json.as_ref())?;
                Ok(true)
            }
            ("POST", "/api/localsend/v2/upload") => match self.upload(req, s, peer) {
                Ok(status) => {
                    http::respond(s.get_mut(), status, None)?;
                    // Refusé avant lecture : le corps n'a pas été lu
                    Ok(status == 200 || status == 422)
                }
                Err(e) => {
                    let _ = http::respond(s.get_mut(), 500, None);
                    Err(e)
                }
            },
            ("POST", "/api/localsend/v2/cancel") => {
                req.body(s).skip(1024 * 1024)?;
                self.cancel(req.param("sessionId").unwrap_or(""), peer);
                http::respond(s.get_mut(), 200, None)?;
                Ok(true)
            }
            _ => {
                let read = req.body(s).skip(1024 * 1024).is_ok();
                http::respond(s.get_mut(), 404, None)?;
                Ok(read)
            }
        }
    }

    fn prepare(&self, v: &Value, peer: IpAddr) -> (u16, Option<Value>) {
        let alias = v["info"]["alias"]
            .as_str()
            .map(str::trim)
            .filter(|a| !a.is_empty())
            .unwrap_or("Un appareil")
            .to_string();
        let Some(files) = v["files"].as_object() else {
            return (400, None);
        };
        let mut offers = Vec::new();
        let mut texts = Vec::new();
        for (key, f) in files {
            // Message texte : son contenu est dans l'aperçu, rien à transférer
            if f["fileType"].as_str().is_some_and(|t| t.starts_with("text/plain"))
                && let Some(text) = f["preview"].as_str()
            {
                texts.push(text.to_string());
                continue;
            }
            offers.push(Offer {
                id: f["id"].as_str().unwrap_or(key).to_string(),
                name: f["fileName"].as_str().unwrap_or("").to_string(),
                size: f["size"].as_u64().unwrap_or(0),
                sha256: f["sha256"]
                    .as_str()
                    .filter(|h| !h.is_empty())
                    .map(str::to_ascii_lowercase),
            });
        }
        {
            let mut st = self.state.lock().unwrap();
            if st.session.as_ref().is_some_and(|s| s.last.elapsed() > IDLE) {
                st.session = None;
            }
            if st.asking || st.session.is_some() {
                return (409, None);
            }
            st.asking = !offers.is_empty();
        }
        for text in texts {
            let alias = alias.clone();
            thread::spawn(move || message(&alias, &text));
        }
        if offers.is_empty() {
            return (204, None);
        }
        let dir = downloads();
        let total: u64 = offers.iter().map(|o| o.size).sum();
        let accepted = if free_space(&dir).is_some_and(|free| free < total) {
            ask::notify(
                0,
                &format!("Envoi de {alias} refusé"),
                &format!(
                    "Pas assez de place pour {} dans Téléchargements.",
                    ask::size(total)
                ),
                None,
            );
            false
        } else {
            let (title, body) = request_text(&alias, &offers, total);
            ask::choice(
                0,
                &title,
                &body,
                &[("accepter", "Accepter"), ("refuser", "Refuser")],
                ASK_TIMEOUT,
                true,
            )
            .as_deref()
                == Some("accepter")
        };
        let mut st = self.state.lock().unwrap();
        st.asking = false;
        if !accepted {
            return (403, None);
        }
        let mut reserved = HashSet::new();
        let mut tops = HashMap::new();
        let mut slots = HashMap::new();
        let mut tokens = serde_json::Map::new();
        for o in offers {
            let token = random();
            tokens.insert(o.id.clone(), token.clone().into());
            slots.insert(
                o.id,
                Slot {
                    token,
                    dest: destination(&dir, &o.name, &mut tops, &mut reserved),
                    sha256: o.sha256,
                    state: SlotState::Waiting,
                },
            );
        }
        let id = random();
        st.session = Some(Session {
            id: id.clone(),
            peer,
            alias,
            slots,
            total,
            received: 0,
            last: Instant::now(),
            progress_at: None,
            notif: 0,
            saved: Vec::new(),
        });
        (200, Some(json!({ "sessionId": id, "files": tokens })))
    }

    fn upload<S: Read>(
        &self,
        req: &Request,
        s: &mut BufReader<S>,
        peer: IpAddr,
    ) -> io::Result<u16> {
        let (Some(sid), Some(fid), Some(token)) = (
            req.param("sessionId"),
            req.param("fileId"),
            req.param("token"),
        ) else {
            return Ok(400);
        };
        let (dest, sha) = {
            let mut st = self.state.lock().unwrap();
            let Some(sess) = st
                .session
                .as_mut()
                .filter(|x| x.id == sid && x.peer == peer)
            else {
                return Ok(403);
            };
            sess.last = Instant::now();
            let Some(slot) = sess.slots.get_mut(fid).filter(|x| x.token == token) else {
                return Ok(403);
            };
            if slot.state != SlotState::Waiting {
                return Ok(409);
            }
            slot.state = SlotState::Writing;
            (slot.dest.clone(), slot.sha256.clone())
        };
        let result = self.write(&dest, req.body(s), sha.as_deref(), sid);
        let status = match result {
            Ok(true) => 200,
            Ok(false) => 422,
            Err(_) => 500,
        };
        let mut st = self.state.lock().unwrap();
        if let Some(sess) = st.session.as_mut().filter(|x| x.id == sid) {
            if let Some(slot) = sess.slots.get_mut(fid) {
                slot.state = if status == 200 {
                    SlotState::Done
                } else {
                    SlotState::Failed
                };
            }
            if status == 200 {
                sess.saved.push(dest);
            }
            if sess
                .slots
                .values()
                .all(|x| matches!(x.state, SlotState::Done | SlotState::Failed))
                && let Some(done) = st.session.take()
            {
                thread::spawn(move || finished(done));
            }
        }
        result.map(|_| status)
    }

    /// Écrit un fichier reçu ; Ok(false) : empreinte différente de l'annoncée
    fn write<R: Read>(
        &self,
        dest: &Path,
        mut body: R,
        sha: Option<&str>,
        sid: &str,
    ) -> io::Result<bool> {
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        let name = dest.file_name().unwrap_or_default().to_string_lossy();
        let tmp = dest.with_file_name(format!(".{name}.reception"));
        let mut out = fs::File::create(&tmp)?;
        let mut hasher = sha.map(|_| Sha256::new());
        let mut buf = vec![0u8; 256 * 1024];
        let mut pending = 0u64;
        let copied = (|| -> io::Result<()> {
            loop {
                let n = body.read(&mut buf)?;
                if n == 0 {
                    return Ok(());
                }
                out.write_all(&buf[..n])?;
                if let Some(h) = hasher.as_mut() {
                    h.update(&buf[..n]);
                }
                pending += n as u64;
                if pending >= 1_000_000 {
                    self.progress(sid, pending);
                    pending = 0;
                }
            }
        })();
        self.progress(sid, pending);
        drop(out);
        if let Err(e) = copied {
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
        if let (Some(h), Some(expected)) = (hasher, sha) {
            let got: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
            if got != expected {
                let _ = fs::remove_file(&tmp);
                return Ok(false);
            }
        }
        fs::rename(&tmp, dest)?;
        Ok(true)
    }

    /// Octets reçus ; jauge dans une notification pour les gros envois
    fn progress(&self, sid: &str, add: u64) {
        let (notif, alias, received, total) = {
            let mut st = self.state.lock().unwrap();
            let Some(sess) = st.session.as_mut().filter(|x| x.id == sid) else {
                return;
            };
            sess.received += add;
            sess.last = Instant::now();
            if sess.total < PROGRESS_MIN
                || sess
                    .progress_at
                    .is_some_and(|t| t.elapsed() < Duration::from_secs(1))
            {
                return;
            }
            sess.progress_at = Some(Instant::now());
            (sess.notif, sess.alias.clone(), sess.received, sess.total)
        };
        let pct = (received.min(total) * 100 / total.max(1)) as u8;
        let nid = ask::notify(
            notif,
            &format!("Réception de {alias}"),
            &format!("{} sur {}", ask::size(received), ask::size(total)),
            Some(pct),
        );
        if let Some(sess) = self
            .state
            .lock()
            .unwrap()
            .session
            .as_mut()
            .filter(|x| x.id == sid)
        {
            sess.notif = nid;
        }
    }

    fn cancel(&self, sid: &str, peer: IpAddr) {
        let sess = {
            let mut st = self.state.lock().unwrap();
            if !st
                .session
                .as_ref()
                .is_some_and(|x| x.id == sid && x.peer == peer)
            {
                return;
            }
            st.session.take()
        };
        if let Some(sess) = sess {
            let kept = match sess.saved.len() {
                0 => String::new(),
                1 => " Un fichier était déjà arrivé, dans Téléchargements.".into(),
                n => format!(" {n} fichiers étaient déjà arrivés, dans Téléchargements."),
            };
            ask::notify(
                sess.notif,
                "Envoi annulé",
                &format!("{} a annulé l'envoi.{kept}", sess.alias),
                None,
            );
        }
    }
}

/// Titre et texte de la demande d'accord
fn request_text(alias: &str, offers: &[Offer], total: u64) -> (String, String) {
    let mut body = match offers {
        [one] => format!("« {} »", leaf(&one.name)),
        _ => format!("{} fichiers", offers.len()),
    };
    body.push_str(&format!(" ({}), dans Téléchargements", ask::size(total)));
    if offers.len() > 1 {
        for o in offers.iter().take(LISTED) {
            body.push_str(&format!("\n{}", o.name));
        }
        if offers.len() > LISTED {
            body.push_str(&format!("\net {} autres", offers.len() - LISTED));
        }
    }
    (format!("Envoi de {alias}"), body)
}

fn leaf(name: &str) -> &str {
    name.rsplit(['/', '\\']).next().unwrap_or(name)
}

/// Fini : une notification ; un clic montre les fichiers
fn finished(sess: Session) {
    let n = sess.saved.len();
    let failed = sess.slots.len() - n;
    if n == 0 {
        ask::notify(
            sess.notif,
            &format!("Envoi de {} échoué", sess.alias),
            "Aucun fichier n'a pu être reçu.",
            None,
        );
        return;
    }
    let dir = downloads();
    let mut body = match sess.saved.as_slice() {
        [one] => one
            .strip_prefix(&dir)
            .unwrap_or(one)
            .to_string_lossy()
            .into_owned(),
        _ => format!("{n} fichiers, dans Téléchargements"),
    };
    if failed > 0 {
        body.push_str(&format!("\n{failed} n'ont pas pu être reçus."));
    }
    // Dossier à montrer : celui des fichiers s'ils sont tous dans le même
    let first = sess.saved[0].parent().unwrap_or(&dir).to_path_buf();
    let target = if sess.saved.iter().all(|p| p.parent() == Some(first.as_path())) {
        first
    } else {
        dir
    };
    let picked = ask::choice(
        sess.notif,
        &format!("Reçu de {}", sess.alias),
        &body,
        &[("default", "Afficher"), ("afficher", "Afficher")],
        Duration::from_secs(3600),
        false,
    );
    if picked.is_some() {
        sibling("files", &target);
    }
}

/// Message texte : affiché, avec un bouton pour le copier
fn message(alias: &str, text: &str) {
    let picked = ask::choice(
        0,
        &format!("Message de {alias}"),
        text,
        &[("copier", "Copier")],
        Duration::from_secs(3600),
        false,
    );
    if picked.as_deref() == Some("copier")
        && let Ok(mut child) = Command::new("wl-copy").stdin(Stdio::piped()).spawn()
    {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        let _ = child.wait();
    }
}

/// Lance une appli d'odysseus (voisine de ce programme) sur un chemin
fn sibling(name: &str, arg: &Path) {
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(name)));
    if let Some(exe) = exe {
        let _ = Command::new(exe).arg(arg).spawn();
    }
}

/// Dossier de réception : Téléchargements (ou Downloads s'il est seul)
pub fn downloads() -> PathBuf {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_else(|| "/tmp".into()));
    let fr = home.join("Téléchargements");
    let en = home.join("Downloads");
    if !fr.is_dir() && en.is_dir() {
        return en;
    }
    let _ = fs::create_dir_all(&fr);
    fr
}

fn free_space(dir: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    (unsafe { libc::statvfs(c.as_ptr(), &mut st) } == 0)
        .then(|| st.f_bavail as u64 * st.f_frsize as u64)
}

/// Identifiant imprévisible (session, jetons des fichiers)
fn random() -> String {
    let mut b = [0u8; 16];
    if fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut b))
        .is_err()
    {
        // Ne devrait pas arriver ; à défaut, l'horloge
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        b = t.to_le_bytes();
    }
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Morceau de chemin sûr : ni « .. », ni séparateur, ni caractère de contrôle
fn clean(part: &str) -> Option<String> {
    let p: String = part.chars().filter(|c| !c.is_control()).collect();
    let p = p.trim().to_string();
    (!p.is_empty() && p != "." && p != "..").then_some(p)
}

/// Où écrire un fichier annoncé (« dossier/sous-dossier/nom » pour un dossier
/// envoyé) : jamais hors du dossier de réception, jamais sur un fichier
/// existant (« nom 2.ext », comme sur le Mac)
fn destination(
    dir: &Path,
    name: &str,
    tops: &mut HashMap<String, String>,
    reserved: &mut HashSet<String>,
) -> PathBuf {
    let mut parts: Vec<String> = name.split(['/', '\\']).filter_map(clean).collect();
    if parts.is_empty() {
        parts.push("fichier".into());
    }
    let top = if parts.len() > 1 {
        match tops.get(&parts[0]) {
            Some(t) => t.clone(),
            None => {
                let t = unique(dir, &parts[0], reserved);
                tops.insert(parts[0].clone(), t.clone());
                t
            }
        }
    } else {
        unique(dir, &parts[0], reserved)
    };
    reserved.insert(top.clone());
    let mut path = dir.join(top);
    for p in &parts[1..] {
        path.push(p);
    }
    path
}

fn unique(dir: &Path, name: &str, reserved: &HashSet<String>) -> String {
    let free = |n: &str| !reserved.contains(n) && fs::symlink_metadata(dir.join(n)).is_err();
    if free(name) {
        return name.to_string();
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => name.split_at(i),
        _ => (name, ""),
    };
    (2..)
        .map(|n| format!("{stem} {n}{ext}"))
        .find(|c| free(c))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destinations() {
        let dir = std::env::temp_dir().join(format!("reception-test-{}", std::process::id()));
        fs::create_dir_all(dir.join("Photos")).unwrap();
        fs::write(dir.join("a.txt"), "").unwrap();
        let (mut tops, mut reserved) = (HashMap::new(), HashSet::new());
        let mut d = |n: &str| destination(&dir, n, &mut tops, &mut reserved);
        assert_eq!(d("a.txt"), dir.join("a 2.txt"));
        assert_eq!(d("a.txt"), dir.join("a 3.txt"));
        assert_eq!(d("../../etc/passwd"), dir.join("etc/passwd"));
        assert_eq!(d("Photos/x.jpg"), dir.join("Photos 2/x.jpg"));
        assert_eq!(d("Photos/y.jpg"), dir.join("Photos 2/y.jpg"));
        assert_eq!(d("/"), dir.join("fichier"));
        assert_eq!(d(".bashrc"), dir.join(".bashrc"));
        fs::remove_dir_all(&dir).unwrap();
    }
}
