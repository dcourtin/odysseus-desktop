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
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
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

/// Notification à boutons ; renvoie la clé du bouton choisi, ou None (fermée,
/// ou pas de réponse dans le délai). Critique : elle reste affichée jusqu'au
/// choix, même en « Ne pas déranger ».
pub fn notify_choice(
    summary: &str,
    body: &str,
    actions: &[(&str, &str)],
    timeout: Duration,
) -> Result<Option<String>, String> {
    let conn = Connection::new_session().map_err(|e| format!("bus de session : {e}"))?;
    // None : en attente ; Some(None) : fermée sans choix ; Some(Some(clé)) : choisi
    let answer: Arc<Mutex<Option<Option<String>>>> = Arc::new(Mutex::new(None));
    let id = Arc::new(AtomicU32::new(0));
    {
        let (answer, id) = (answer.clone(), id.clone());
        conn.add_match(
            // Signal venu du seul démon de notifications : un autre programme ne peut pas répondre à sa place
            MatchRule::new_signal("org.freedesktop.Notifications", "ActionInvoked")
                .with_sender("org.freedesktop.Notifications"),
            move |(nid, key): (u32, String), _, _| {
                if nid == id.load(Ordering::Relaxed) {
                    *answer.lock().unwrap() = Some(Some(key));
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
                let mut a = answer.lock().unwrap();
                if nid == id.load(Ordering::Relaxed) && a.is_none() {
                    *a = Some(None);
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
    hints.insert("urgency", Variant(Box::new(2u8)));
    let flat: Vec<&str> = actions.iter().flat_map(|(k, l)| [*k, *l]).collect();
    let (nid,): (u32,) = proxy
        .method_call(
            "org.freedesktop.Notifications",
            "Notify",
            ("Assistant", 0u32, "", summary, body, flat, hints, 0i32),
        )
        .map_err(|e| format!("notifications : {e}"))?;
    id.store(nid, Ordering::Relaxed);
    let start = Instant::now();
    while answer.lock().unwrap().is_none() && start.elapsed() < timeout {
        conn.process(Duration::from_millis(200))
            .map_err(|e| e.to_string())?;
    }
    // Réponse donnée (ou délai passé) : la notification s'en va
    let _: Result<(), _> =
        proxy.method_call("org.freedesktop.Notifications", "CloseNotification", (nid,));
    Ok(answer.lock().unwrap().clone().flatten())
}

/// Demande l'accord de l'utilisateur ; true s'il autorise
fn ask(command: &str, dir: &str, reason: &str) -> Result<bool, String> {
    let body = format!(
        "{}{command}\n\nDans {dir}, avec vos droits.",
        if reason.trim().is_empty() {
            String::new()
        } else {
            format!("{}\n\n", reason.trim())
        }
    );
    let key = notify_choice(
        "Commande de l'agent",
        &body,
        &[("autoriser", "Autoriser"), ("refuser", "Refuser")],
        ASK_TIMEOUT,
    )?;
    Ok(key.as_deref() == Some("autoriser"))
}

/// Une question et jusqu'à trois réponses ; l'indice choisi, ou None
pub fn choose(title: &str, question: &str, choices: &[String]) -> Result<Option<usize>, String> {
    let keys: Vec<String> = (0..choices.len()).map(|i| i.to_string()).collect();
    let actions: Vec<(&str, &str)> = keys
        .iter()
        .zip(choices)
        .map(|(k, c)| (k.as_str(), c.as_str()))
        .collect();
    let key = notify_choice(title, question, &actions, Duration::from_secs(300))?;
    Ok(key
        .and_then(|k| k.parse().ok())
        .filter(|i| *i < choices.len()))
}

/// Lance une commande ; sans `approved` (automatisme validé), l'utilisateur
/// l'autorise d'abord dans une notification
pub fn execute(
    command: &str,
    dir: Option<&str>,
    reason: &str,
    approved: bool,
) -> Result<String, String> {
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
    if !approved && !ask(command, &dir, reason)? {
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
