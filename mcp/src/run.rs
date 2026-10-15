//! Commandes lancées par l'agent (catégorie « Exécuter des commandes ») :
//! chacune est d'abord montrée à l'utilisateur dans une notification
//! « Autoriser / Refuser » ; sans réponse en 90 s, c'est non. Lancées avec
//! les droits de l'utilisateur (jamais root), sans entrée, 2 minutes au plus.

use std::{
    collections::HashMap,
    io::Read,
    os::unix::process::CommandExt,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicU8, AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};

use dbus::{
    arg::{RefArg, Variant},
    blocking::Connection,
    message::MatchRule,
};

const ASK_TIMEOUT: Duration = Duration::from_secs(90);
const RUN_TIMEOUT: Duration = Duration::from_secs(120);
const OUTPUT_LIMIT: usize = 20_000;

/// Réponse de l'utilisateur : en attente, oui, non
const PENDING: u8 = 0;
const YES: u8 = 1;
const NO: u8 = 2;

/// Demande l'accord de l'utilisateur ; true s'il autorise
fn ask(command: &str, dir: &str, reason: &str) -> Result<bool, String> {
    let conn = Connection::new_session().map_err(|e| format!("bus de session : {e}"))?;
    let answer = Arc::new(AtomicU8::new(PENDING));
    let id = Arc::new(AtomicU32::new(0));
    {
        let (answer, id) = (answer.clone(), id.clone());
        conn.add_match(
            // Signal venu du seul démon de notifications : un autre programme ne peut pas répondre à sa place
            MatchRule::new_signal("org.freedesktop.Notifications", "ActionInvoked")
                .with_sender("org.freedesktop.Notifications"),
            move |(nid, key): (u32, String), _, _| {
                if nid == id.load(Ordering::Relaxed) {
                    answer.store(if key == "autoriser" { YES } else { NO }, Ordering::Relaxed);
                }
                true
            },
        )
        .map_err(|e| e.to_string())?;
    }
    {
        let (answer, id) = (answer.clone(), id.clone());
        conn.add_match(
            MatchRule::new_signal("org.freedesktop.Notifications", "NotificationClosed")
                .with_sender("org.freedesktop.Notifications"),
            move |(nid, _reason): (u32, u32), _, _| {
                // Fermée sans choisir : refus
                if nid == id.load(Ordering::Relaxed) {
                    let _ =
                        answer.compare_exchange(PENDING, NO, Ordering::Relaxed, Ordering::Relaxed);
                }
                true
            },
        )
        .map_err(|e| e.to_string())?;
    }
    let proxy = conn.with_proxy(
        "org.freedesktop.Notifications",
        "/org/freedesktop/Notifications",
        Duration::from_secs(5),
    );
    let mut hints: HashMap<&str, Variant<Box<dyn RefArg>>> = HashMap::new();
    // Critique : reste affichée jusqu'au choix, même en « Ne pas déranger »
    hints.insert("urgency", Variant(Box::new(2u8)));
    let body = format!(
        "{}{command}\n\nDans {dir}, avec vos droits.",
        if reason.trim().is_empty() {
            String::new()
        } else {
            format!("{}\n\n", reason.trim())
        }
    );
    let (nid,): (u32,) = proxy
        .method_call(
            "org.freedesktop.Notifications",
            "Notify",
            (
                "Assistant",
                0u32,
                "utilities-terminal",
                "Commande de l'agent",
                body,
                vec!["autoriser", "Autoriser", "refuser", "Refuser"],
                hints,
                0i32,
            ),
        )
        .map_err(|e| format!("notifications : {e}"))?;
    id.store(nid, Ordering::Relaxed);
    let start = Instant::now();
    while answer.load(Ordering::Relaxed) == PENDING && start.elapsed() < ASK_TIMEOUT {
        conn.process(Duration::from_millis(200))
            .map_err(|e| e.to_string())?;
    }
    // Réponse donnée (ou délai passé) : la notification s'en va
    {
        let _: Result<(), _> =
            proxy.method_call("org.freedesktop.Notifications", "CloseNotification", (nid,));
    }
    Ok(answer.load(Ordering::Relaxed) == YES)
}

pub fn execute(command: &str, dir: Option<&str>, reason: &str) -> Result<String, String> {
    let command = command.trim();
    if command.is_empty() {
        return Err("Commande vide".into());
    }
    if command.chars().count() > 2000 {
        return Err("Commande trop longue (2 000 caractères au plus)".into());
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
    let dir = match dir.filter(|d| !d.trim().is_empty()) {
        Some(d) if d.starts_with("~/") => format!("{home}/{}", &d[2..]),
        Some(d) => d.to_string(),
        None => home,
    };
    if !std::path::Path::new(&dir).is_dir() {
        return Err(format!("Dossier introuvable : {dir}"));
    }
    if !ask(command, &dir, reason)? {
        return Err(
            "Refusé par l'utilisateur (ou pas de réponse). Ne relancez pas la même commande \
             sans lui demander pourquoi."
                .into(),
        );
    }
    // Les outils de l'utilisateur, comme dans son terminal (rustup, AppImage…)
    let user_bins: Vec<String> = [".local/bin", ".cargo/bin"]
        .iter()
        .map(|d| format!("{}/{d}", std::env::var("HOME").unwrap_or_default()))
        .filter(|d| std::path::Path::new(d).is_dir())
        .collect();
    let path = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into());
    let path = if user_bins.is_empty() {
        path
    } else {
        format!("{}:{path}", user_bins.join(":"))
    };
    let mut child = Command::new("/bin/sh")
        .arg("-c")
        // Erreurs et sortie mêlées, dans l'ordre
        .arg(format!("exec 2>&1\n{command}"))
        .current_dir(&dir)
        .env("PATH", path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|e| format!("sh : {e}"))?;
    let mut stdout = child.stdout.take().ok_or("sortie absente")?;
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    let start = Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait().map_err(|e| e.to_string())? {
            break Some(s);
        }
        if start.elapsed() > RUN_TIMEOUT {
            // Tout le groupe : la commande et ce qu'elle a lancé
            unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let raw = reader.join().unwrap_or_default();
    let text = String::from_utf8_lossy(&raw);
    let total = text.chars().count();
    // Trop long : le début et la fin, souvent les plus parlants
    let shown = if total > OUTPUT_LIMIT {
        let head: String = text.chars().take(OUTPUT_LIMIT / 2).collect();
        let tail: String = text.chars().skip(total - OUTPUT_LIMIT / 2).collect();
        format!(
            "{head}\n[… {} caractères omis …]\n{tail}",
            total - OUTPUT_LIMIT
        )
    } else {
        text.into_owned()
    };
    let head = match status {
        Some(s) => match s.code() {
            Some(0) => "Terminée (code 0)".to_string(),
            Some(c) => format!("Terminée avec le code {c}"),
            None => "Interrompue par un signal".to_string(),
        },
        None => format!("Arrêtée au bout de {} s", RUN_TIMEOUT.as_secs()),
    };
    Ok(format!(
        "{head}, en {:.1} s.\n{}",
        start.elapsed().as_secs_f32(),
        if shown.trim().is_empty() {
            "(aucune sortie)".to_string()
        } else {
            shown
        }
    ))
}
