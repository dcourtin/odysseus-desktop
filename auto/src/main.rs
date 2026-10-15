//! odysseus-auto : automatismes du bureau.
//!
//!     odysseus-auto                       démon : déclencheurs « heure » et « dossier »
//!     odysseus-auto lancer <nom> [--fichier <chemin>] [--auto]
//!     odysseus-auto liste
//!
//! Le démon (lancé avec la session) surveille les automatismes validés :
//! à l'heure dite, ou quand un fichier arrive dans un dossier surveillé, il
//! lance « odysseus-auto lancer » dans un processus à part. Chaque
//! exécution est notée (Réglages > Automatismes) ; un échec est notifié.

mod engine;

use std::{
    collections::HashMap,
    ffi::CString,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    process::{Child, Command},
    time::{Duration, Instant},
};

use chrono::{Datelike, Timelike};
use common::auto::{self, Automatism, Trigger};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("lancer") => {
            let Some(key) = args.get(1) else {
                eprintln!("usage : odysseus-auto lancer <nom> [--fichier <chemin>]");
                std::process::exit(2);
            };
            let file = args
                .iter()
                .position(|a| a == "--fichier")
                .and_then(|i| args.get(i + 1))
                .map(String::as_str);
            let automatic = args.iter().any(|a| a == "--auto");
            std::process::exit(match launch(key, file, automatic) {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("odysseus-auto : {e}");
                    1
                }
            });
        }
        Some("liste") => {
            let (all, bad) = auto::load_all();
            for a in all {
                let st = auto::status(&a);
                println!(
                    "{}\t{}\t{}",
                    a.id,
                    a.name,
                    match (st.validated, st.paused) {
                        (false, _) => "à valider",
                        (true, true) => "en pause",
                        (true, false) => "actif",
                    }
                );
            }
            for (id, e) in bad {
                println!("{id}\t(illisible : {e})");
            }
        }
        None | Some("--demon") => daemon(),
        Some(other) => {
            eprintln!("odysseus-auto : commande inconnue « {other} » (lancer, liste)");
            std::process::exit(2);
        }
    }
}

/// Lance un automatisme validé (et pas en pause, s'il est déclenché tout seul)
fn launch(key: &str, file: Option<&str>, automatic: bool) -> Result<(), String> {
    let a = auto::find(key).ok_or_else(|| format!("automatisme inconnu : {key}"))?;
    let st = auto::status(&a);
    if !st.validated {
        return Err(format!(
            "« {} » n'est pas validé (Réglages > Automatismes)",
            a.name
        ));
    }
    if automatic && st.paused {
        return Ok(());
    }
    let mut ctx = engine::Ctx::new(&a, file);
    let outcome = engine::run_steps(&a.steps, &mut ctx, "");
    match &outcome {
        Ok(()) => auto::record_run(&a.id, "réussi"),
        Err(e) => {
            auto::record_run(&a.id, &format!("échec : {e}"));
            let _ = Command::new(sibling("odysseus-notify"))
                .args(["--send", &format!("Automatisme « {} »", a.name), e])
                .status();
        }
    }
    outcome
}

/// Ce programme ; après une mise à jour, /proc/self/exe se termine par
/// « (deleted) » : c'est le nouveau fichier, au même endroit, qu'on veut
fn own_exe() -> PathBuf {
    let p = std::env::current_exe().unwrap_or_else(|_| "odysseus-auto".into());
    let s = p.to_string_lossy();
    match s.strip_suffix(" (deleted)") {
        Some(clean) => PathBuf::from(clean),
        None => p,
    }
}

fn sibling(name: &str) -> PathBuf {
    own_exe()
        .parent()
        .map(|d| d.join(name))
        .unwrap_or_else(|| PathBuf::from(name))
}

// --- Démon ---------------------------------------------------------------------------

/// Une seule instance par session : fichier .pid dans XDG_RUNTIME_DIR
fn single_instance() -> bool {
    let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") else {
        return true;
    };
    let path = PathBuf::from(dir).join("odysseus-auto.pid");
    if let Some(pid) = std::fs::read_to_string(&path)
        .ok()
        .and_then(|p| p.trim().parse::<i32>().ok())
        && pid != std::process::id() as i32
        && Path::new(&format!("/proc/{pid}/exe"))
            .read_link()
            .is_ok_and(|exe| {
                exe.file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("odysseus-auto"))
            })
    {
        return false;
    }
    std::fs::write(path, std::process::id().to_string()).is_ok()
}

struct Watcher {
    fd: i32,
    /// Descripteur de surveillance → dossier (None : dossier des automatismes)
    watches: HashMap<i32, Option<PathBuf>>,
}

impl Watcher {
    fn new() -> Option<Self> {
        let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        (fd >= 0).then(|| Self {
            fd,
            watches: HashMap::new(),
        })
    }

    fn add(&mut self, dir: &Path, target: Option<PathBuf>) {
        let Ok(c) = CString::new(dir.as_os_str().as_bytes()) else {
            return;
        };
        let mask = libc::IN_CLOSE_WRITE | libc::IN_MOVED_TO | libc::IN_CREATE | libc::IN_DELETE;
        let wd = unsafe { libc::inotify_add_watch(self.fd, c.as_ptr(), mask) };
        if wd >= 0 {
            self.watches.insert(wd, target);
        }
    }

    /// Surveille le dossier des automatismes et ceux de leurs déclencheurs
    fn reset(&mut self, folders: &[PathBuf]) {
        for wd in self.watches.keys() {
            unsafe { libc::inotify_rm_watch(self.fd, *wd) };
        }
        self.watches.clear();
        let dir = auto::dir();
        let _ = std::fs::create_dir_all(&dir);
        self.add(&dir, None);
        for f in folders {
            self.add(f, Some(f.clone()));
        }
    }

    /// Attend un événement (au plus `timeout`) : (dossier surveillé, nom, fini d'écrire)
    fn wait(&self, timeout: Duration) -> Vec<(Option<PathBuf>, String, bool)> {
        let mut pfd = libc::pollfd {
            fd: self.fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let ms = timeout.as_millis().min(i32::MAX as u128) as i32;
        if unsafe { libc::poll(&mut pfd, 1, ms) } <= 0 {
            return Vec::new();
        }
        let mut out = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            let n = unsafe { libc::read(self.fd, buf.as_mut_ptr().cast(), buf.len()) };
            if n <= 0 {
                break;
            }
            let mut off = 0usize;
            let header = std::mem::size_of::<libc::inotify_event>();
            while off + header <= n as usize {
                let ev: libc::inotify_event =
                    unsafe { std::ptr::read_unaligned(buf[off..].as_ptr().cast()) };
                let name_bytes = &buf[off + header..off + header + ev.len as usize];
                let name = String::from_utf8_lossy(name_bytes)
                    .trim_end_matches('\0')
                    .to_string();
                let done = ev.mask & (libc::IN_CLOSE_WRITE | libc::IN_MOVED_TO) != 0;
                if let Some(target) = self.watches.get(&ev.wd) {
                    out.push((target.clone(), name, done));
                }
                off += header + ev.len as usize;
            }
        }
        out
    }
}

/// Fichier en cours de téléchargement ou temporaire : on attend la suite
fn temporary(name: &str) -> bool {
    name.starts_with('.')
        || name.ends_with('~')
        || [".part", ".crdownload", ".tmp", ".download", ".swp"]
            .iter()
            .any(|e| name.ends_with(e))
}

fn daemon() {
    if !single_instance() {
        eprintln!("odysseus-auto : déjà lancé");
        return;
    }
    let Some(mut watcher) = Watcher::new() else {
        eprintln!("odysseus-auto : inotify indisponible");
        return;
    };
    let mut list: Vec<Automatism> = Vec::new();
    let mut reload = true;
    let mut last_minute: Option<(u32, u32, u32)> = None;
    let mut children: Vec<Child> = Vec::new();
    // Même fichier vu deux fois (création puis écriture) : une seule exécution
    let mut seen: HashMap<PathBuf, Instant> = HashMap::new();
    let mut runs: HashMap<String, Vec<Instant>> = HashMap::new();

    loop {
        if reload {
            list = auto::load_all().0;
            let mut folders: Vec<PathBuf> = list
                .iter()
                .flat_map(|a| a.triggers.iter())
                .filter_map(|t| match t {
                    Trigger::Folder(d) if d.is_dir() => Some(d.clone()),
                    _ => None,
                })
                .collect();
            folders.sort();
            folders.dedup();
            watcher.reset(&folders);
            reload = false;
            eprintln!(
                "odysseus-auto : {} automatisme(s), {} dossier(s) surveillé(s)",
                list.len(),
                folders.len()
            );
        }

        // Réveil au plus tard au début de la minute suivante
        let now = chrono::Local::now();
        let wait = Duration::from_millis(
            (60 - now.second() as u64) * 1000 - now.timestamp_subsec_millis().min(999) as u64,
        );
        let events = watcher.wait(wait);
        children.retain_mut(|c| c.try_wait().map(|s| s.is_none()).unwrap_or(false));
        seen.retain(|_, t| t.elapsed() < Duration::from_secs(10));

        let mut fire: Vec<(String, Option<String>)> = Vec::new();
        for (target, name, done) in events {
            match target {
                // Un automatisme ajouté, modifié ou supprimé : on relit tout
                None => reload = true,
                Some(dir) if done && !name.is_empty() && !temporary(&name) => {
                    let path = dir.join(&name);
                    if !path.is_file() || seen.contains_key(&path) {
                        continue;
                    }
                    seen.insert(path.clone(), Instant::now());
                    for a in &list {
                        if a.triggers.contains(&Trigger::Folder(dir.clone())) {
                            fire.push((a.id.clone(), Some(path.to_string_lossy().into_owned())));
                        }
                    }
                }
                _ => {}
            }
        }

        // Déclencheurs « heure » : une fois par minute
        let now = chrono::Local::now();
        let minute = (now.ordinal(), now.hour(), now.minute());
        if last_minute != Some(minute) {
            if last_minute.is_some() {
                let day = now.weekday().num_days_from_monday();
                for a in &list {
                    let due = a.triggers.iter().any(|t| match t {
                        Trigger::Time { hour, minute, days } => {
                            *hour == now.hour()
                                && *minute == now.minute()
                                && (days.is_empty() || days.contains(&day))
                        }
                        _ => false,
                    });
                    if due {
                        fire.push((a.id.clone(), None));
                    }
                }
            }
            last_minute = Some(minute);
        }

        for (id, file) in fire {
            // Garde-fou : 20 exécutions par minute au plus pour un automatisme
            let history = runs.entry(id.clone()).or_default();
            history.retain(|t| t.elapsed() < Duration::from_secs(60));
            if history.len() >= 20 {
                continue;
            }
            history.push(Instant::now());
            let mut cmd = Command::new(own_exe());
            cmd.args(["lancer", &id, "--auto"]);
            if let Some(f) = &file {
                cmd.args(["--fichier", f]);
            }
            match cmd.spawn() {
                Ok(child) => children.push(child),
                Err(e) => eprintln!("odysseus-auto : {id} ({e})"),
            }
        }
    }
}
