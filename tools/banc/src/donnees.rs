//! Données de démonstration du banc d'essai, écrites dans le HOME courant
//! (celui du banc, jamais le vôtre : le script le vérifie) : trois
//! conversations dans le fil de l'Assistant, des faits dans la mémoire avec
//! leurs dates et origines, un journal d'activité des agents, quelques
//! applications pour le lanceur, des fichiers pour Fichiers. Tout passe par
//! les bibliothèques du dépôt : les formats restent justes quand ils bougent.

use std::{fs, path::PathBuf};

use assistant::store::{Conversation, Kind, Message, Store};
use common::memory::Memory;
use serde_json::json;

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").expect("HOME"))
}

fn msg(kind: Kind, body: &str, extra: &str) -> Message {
    Message {
        kind,
        body: body.into(),
        extra: extra.into(),
    }
}

/// Le fil : trois conversations, la plus récente d'il y a cinq heures
fn conversations() -> Result<(), String> {
    let store = Store::open()?;
    let t = now();
    let convs: [(&str, i64, Vec<Message>); 3] = [
        (
            "claude",
            t - 3600 * 5,
            vec![
                msg(
                    Kind::User,
                    "Pourquoi Archives est lent ?",
                    "contexte: fenêtre, dossier",
                ),
                msg(Kind::Tool, "Mesure du système", "ok"),
                msg(Kind::Tool, "Contenu d'un dossier", "ok"),
                msg(
                    Kind::Answer,
                    "Le dossier Archives est monté en SMB depuis le NAS, et la mesure montre 2,1 Mo/s \
                     en lecture : c'est le Wi-Fi qui limite, pas le disque.\n\n- Le NAS répond en 0,4 ms, \
                     donc le réseau local est sain.\n- La carte Wi-Fi négocie 54 Mbit/s : elle est \
                     accrochée en 2,4 GHz.\n\nBrancher l'Ethernet ou forcer le 5 GHz dans le centre de \
                     contrôle devrait multiplier le débit par dix.",
                    "",
                ),
                msg(Kind::User, "Retiens que c'est un NAS Synology", ""),
                msg(Kind::Tool, "Souvenir soumis à votre accord", "ok"),
                msg(
                    Kind::Answer,
                    "C'est retenu : « Le disque Archives est un NAS Synology sur le réseau local ». \
                     Vous pouvez l'oublier dans Réglages > Agent > Mémoire.",
                    "",
                ),
            ],
        ),
        (
            "claude",
            t - 86_400 * 5,
            vec![
                msg(Kind::User, "Résume mes notifications", ""),
                msg(Kind::Tool, "Notifications", "ok"),
                msg(
                    Kind::Answer,
                    "Trois choses depuis ce matin : une mise à jour de Firefox prête, deux fichiers \
                     reçus de l'iPhone dans Téléchargements, et la sauvegarde de 6 h terminée sans erreur.",
                    "",
                ),
            ],
        ),
        (
            "ollama",
            t - 86_400 * 6,
            vec![
                msg(
                    Kind::User,
                    "Quel langage pour un script de sauvegarde ?",
                    "",
                ),
                msg(
                    Kind::Answer,
                    "Python : vous l'avez déjà sous la main, et sa bibliothèque standard couvre les \
                     dates, les chemins et les archives. Un script d'une cinquantaine de lignes suffit \
                     pour rsync vers Archives puis supprimer les versions de plus de 30 jours.",
                    "",
                ),
            ],
        ),
    ];
    let db = rusqlite::Connection::open(assistant::store::path()).map_err(|e| e.to_string())?;
    for (agent, when, messages) in convs {
        let id = store.create(agent)?;
        store.save(
            id,
            &Conversation {
                agent: agent.into(),
                claude_session: None,
                ollama_history: Vec::new(),
                messages,
            },
        )?;
        // Les dates : save() met « maintenant », le banc veut un fil qui a vécu
        db.execute(
            "UPDATE conversations SET created = ?1, modified = ?1 WHERE id = ?2",
            rusqlite::params![when, id],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// La mémoire : quatre faits retenus en conversation, dont un vieux de sept
/// mois (à relire), et un écrit à la main
fn memory() -> Result<(), String> {
    let m = Memory::open().ok_or("HOME inconnu")?;
    let t = now();
    let facts = [
        (
            "Utilise Firefox, avec les onglets verticaux",
            "conversation « Firefox rame depuis ce matin »",
            t - 86_400 * 215,
        ),
        (
            "Développe en PHP, Python et JavaScript",
            "conversation « Quel langage pour un script de sauvegarde ? »",
            t - 86_400 * 6,
        ),
        (
            "Préfère les réponses courtes, sans tableaux",
            "conversation « Résume mes notifications »",
            t - 86_400 * 5,
        ),
        (
            "Le disque « Archives » est un NAS Synology sur le réseau local",
            "conversation « Pourquoi Archives est lent ? »",
            t - 3600 * 5,
        ),
    ];
    for (text, origin, _) in &facts {
        m.add(text, Some(origin))?;
    }
    // Les dates de l'état (~/.local/state/odysseus/memoire.json) : étalées
    let state = home().join(".local/state/odysseus/memoire.json");
    if let Ok(raw) = fs::read_to_string(&state)
        && let Ok(mut v) = serde_json::from_str::<serde_json::Value>(&raw)
    {
        for (text, _, when) in &facts {
            v[*text]["date"] = json!(when);
        }
        fs::write(&state, serde_json::to_string_pretty(&v).unwrap_or_default())
            .map_err(|e| e.to_string())?;
    }
    // Un fait ajouté à la main, dans le fichier, sans état
    let mut text = fs::read_to_string(m.file()).map_err(|e| e.to_string())?;
    text.push_str("- Travaille sur odysseus-desktop, un bureau Wayland en Rust\n");
    fs::write(m.file(), text).map_err(|e| e.to_string())
}

/// Le journal d'odysseus-mcp, tel que Réglages > Agent le montre
fn activity() -> Result<(), String> {
    let t = now();
    let lines = [
        (t - 7200, "accordé", "etat_systeme", ""),
        (
            t - 7100,
            "accordé",
            "retenir",
            "fait=Le disque « Archives » est un NAS Synology sur le réseau local",
        ),
        (
            t - 3600,
            "refusé",
            "executer",
            "commande=rm -rf ~/Téléchargements/*.part — refusé par l'utilisateur",
        ),
        (t - 1800, "accordé", "conversations", "chercher=NAS"),
        (t - 600, "accordé", "memoire", ""),
    ];
    let path = home().join(".local/state/odysseus/agent.log");
    fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    let text: String = lines
        .iter()
        .map(|(when, ok, tool, detail)| {
            let stamp = chrono_like(*when);
            format!("{stamp}\t{ok}\t{tool}\t{detail}\n")
        })
        .collect();
    fs::write(path, text).map_err(|e| e.to_string())
}

/// « AAAA-MM-JJTHH:MM:SS » en heure locale, sans dépendance
fn chrono_like(t: i64) -> String {
    let out = std::process::Command::new("date")
        .arg("-d")
        .arg(format!("@{t}"))
        .arg("+%Y-%m-%dT%H:%M:%S")
        .output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => t.to_string(),
    }
}

/// Des applications pour le lanceur et la rangée « Ouvrir … » de l'Assistant
fn applications() -> Result<(), String> {
    let dir = home().join(".local/share/applications");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let apps = [
        ("firefox", "Firefox", "Navigateur web", "firefox %u"),
        (
            "odysseus-files",
            "Fichiers",
            "Explorateur d'odysseus",
            "files",
        ),
        (
            "odysseus-notes",
            "Notes",
            "Notes d'odysseus",
            "odysseus-notes",
        ),
        (
            "odysseus-reglages",
            "Réglages",
            "Réglages d'odysseus",
            "odysseus-reglages",
        ),
        ("foot", "Terminal", "Terminal", "foot"),
        ("org.gimp.GIMP", "GIMP", "Retouche d'images", "gimp %U"),
    ];
    for (id, name, comment, exec) in apps {
        fs::write(
            dir.join(format!("{id}.desktop")),
            format!(
                "[Desktop Entry]\nType=Application\nName={name}\nComment={comment}\nExec={exec}\nIcon={id}\n"
            ),
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// De quoi remplir Fichiers
fn files() -> Result<(), String> {
    let h = home();
    for (dir, names) in [
        (
            "Téléchargements",
            vec![
                ("rapport-trimestriel-2026.pdf", 4_200_000usize),
                ("IMG_0412.jpg", 2_800_000),
                ("IMG_0413.jpg", 3_100_000),
                ("odysseus-0.1.0.tar.gz", 912_000),
            ],
        ),
        (
            "Documents",
            vec![("Notes de construction.md", 18_000), ("budget.ods", 44_000)],
        ),
        (
            "Projets/odysseus",
            vec![("Cargo.toml", 1_200), ("README.md", 25_000)],
        ),
        (
            "Images",
            vec![("Capture d’écran 2026-10-16 à 09.12.03.png", 1_900_000)],
        ),
    ] {
        let d = h.join(dir);
        fs::create_dir_all(&d).map_err(|e| e.to_string())?;
        for (name, size) in names {
            // Taille réaliste sans contenu : fichier creux
            let f = fs::File::create(d.join(name)).map_err(|e| e.to_string())?;
            f.set_len(size as u64).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn main() {
    let h = home();
    // Garde-fou : jamais dans le vrai HOME de quelqu'un
    if !h.to_string_lossy().contains("banc") {
        eprintln!(
            "banc-donnees : HOME doit être celui du banc (contenir « banc »), pas {}",
            h.display()
        );
        std::process::exit(2);
    }
    for (what, result) in [
        ("conversations", conversations()),
        ("mémoire", memory()),
        ("activité", activity()),
        ("applications", applications()),
        ("fichiers", files()),
    ] {
        match result {
            Ok(()) => println!("banc-donnees : {what} ✓"),
            Err(e) => {
                eprintln!("banc-donnees : {what} : {e}");
                std::process::exit(1);
            }
        }
    }
}
