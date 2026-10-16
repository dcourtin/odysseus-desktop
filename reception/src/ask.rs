//! Notifications de la réception : demande d'accord, progression, résultat.

use std::{
    collections::HashMap,
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

const APP: &str = "Réception";
const DEST: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";

type Hints = HashMap<&'static str, Variant<Box<dyn RefArg>>>;

fn post(
    conn: &Connection,
    replaces: u32,
    summary: &str,
    body: &str,
    actions: &[(&str, &str)],
    hints: Hints,
) -> Result<u32, String> {
    let proxy = conn.with_proxy(DEST, PATH, Duration::from_secs(5));
    let flat: Vec<&str> = actions.iter().flat_map(|(k, l)| [*k, *l]).collect();
    let (id,): (u32,) = proxy
        .method_call(
            DEST,
            "Notify",
            (APP, replaces, "", summary, body, flat, hints, -1i32),
        )
        .map_err(|e| format!("notifications : {e}"))?;
    Ok(id)
}

/// Notification simple (remplace `replaces` si non nul) ; `value` : jauge en %
pub fn notify(replaces: u32, summary: &str, body: &str, value: Option<u8>) -> u32 {
    let Ok(conn) = Connection::new_session() else {
        return 0;
    };
    let mut hints = Hints::new();
    if let Some(v) = value {
        hints.insert("value", Variant(Box::new(v as i32)));
    }
    post(&conn, replaces, summary, body, &[], hints).unwrap_or(0)
}

/// Notification à boutons ; renvoie la clé du bouton choisi, ou None (fermée,
/// ou pas de réponse dans le délai). `urgent` : elle reste affichée jusqu'au
/// choix, même en « Ne pas déranger », et s'en va après.
pub fn choice(
    replaces: u32,
    summary: &str,
    body: &str,
    actions: &[(&str, &str)],
    timeout: Duration,
    urgent: bool,
) -> Option<String> {
    let conn = Connection::new_session().ok()?;
    // None : en attente ; Some(None) : fermée sans choix ; Some(Some(clé)) : choisi
    let answer: Arc<Mutex<Option<Option<String>>>> = Arc::new(Mutex::new(None));
    let id = Arc::new(AtomicU32::new(0));
    {
        let (answer, id) = (answer.clone(), id.clone());
        conn.add_match(
            // Signal venu du seul démon de notifications
            MatchRule::new_signal(DEST, "ActionInvoked").with_sender(DEST),
            move |(nid, key): (u32, String), _, _| {
                if nid == id.load(Ordering::Relaxed) {
                    *answer.lock().unwrap() = Some(Some(key));
                }
                true
            },
        )
        .ok()?;
    }
    {
        let (answer, id) = (answer.clone(), id.clone());
        conn.add_match(
            MatchRule::new_signal(DEST, "NotificationClosed").with_sender(DEST),
            move |(nid, _reason): (u32, u32), _, _| {
                let mut a = answer.lock().unwrap();
                if nid == id.load(Ordering::Relaxed) && a.is_none() {
                    *a = Some(None);
                }
                true
            },
        )
        .ok()?;
    }
    let mut hints = Hints::new();
    if urgent {
        hints.insert("urgency", Variant(Box::new(2u8)));
    }
    let nid = post(&conn, replaces, summary, body, actions, hints).ok()?;
    id.store(nid, Ordering::Relaxed);
    let start = Instant::now();
    while answer.lock().unwrap().is_none() && start.elapsed() < timeout {
        if conn.process(Duration::from_millis(200)).is_err() {
            break;
        }
    }
    if urgent {
        let proxy = conn.with_proxy(DEST, PATH, Duration::from_secs(5));
        let _: Result<(), _> = proxy.method_call(DEST, "CloseNotification", (nid,));
    }
    answer.lock().unwrap().clone().flatten()
}

/// Taille lisible : « 12,4 Mo »
pub fn size(bytes: u64) -> String {
    let units = ["octets", "Ko", "Mo", "Go", "To"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1000.0 && u + 1 < units.len() {
        v /= 1000.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes} octets")
    } else if v < 10.0 {
        format!("{:.1} {}", v, units[u]).replace('.', ",")
    } else {
        format!("{:.0} {}", v, units[u])
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn sizes() {
        assert_eq!(super::size(512), "512 octets");
        assert_eq!(super::size(12_400_000), "12 Mo");
        assert_eq!(super::size(1_260_000), "1,3 Mo");
    }
}
