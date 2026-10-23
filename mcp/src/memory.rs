//! La mémoire pour les agents (catégorie « Mémoire ») : les faits que
//! l'utilisateur a accepté de retenir (common::memory) et le fil des
//! conversations de l'Assistant (assistant::store). Retenir ou oublier un
//! fait passe par une notification : l'agent propose, l'utilisateur décide.

use std::time::Duration;

use assistant::store::{Kind, Store};
use common::memory::{self, Fact, Memory};

use crate::run::notify_choice;

const ASK_TIMEOUT: Duration = Duration::from_secs(90);
/// Conversations montrées dans la liste
const MAX_LISTED: usize = 20;
/// Une conversation lue : au-delà, le début puis une marque
const MAX_READ: usize = 12_000;

fn open() -> Result<Memory, String> {
    Memory::open().ok_or_else(|| "HOME inconnu".into())
}

fn date(t: i64) -> String {
    chrono::DateTime::from_timestamp(t, 0)
        .map(|d| {
            d.with_timezone(&chrono::Local)
                .format("%d/%m/%Y %H:%M")
                .to_string()
        })
        .unwrap_or_default()
}

fn meta(f: &Fact) -> String {
    match (f.date, &f.origin) {
        (None, None) => " (écrit à la main)".into(),
        (Some(t), None) => format!(" (retenu le {})", date(t)),
        (Some(t), Some(o)) => format!(" (retenu le {}, {o})", date(t)),
        (None, Some(o)) => format!(" ({o})"),
    }
}

pub fn facts() -> Result<String, String> {
    let m = open()?;
    let facts = m.facts();
    if facts.is_empty() {
        return Ok(format!(
            "La mémoire est vide ({}). Quand l'utilisateur vous confie quelque chose de durable \
             sur lui ou sa machine, proposez de le retenir (outil retenir) : il décide.",
            m.file().display()
        ));
    }
    let mut out = format!(
        "Ce que l'utilisateur a accepté de retenir ({} fait{}, {}) :\n",
        facts.len(),
        if facts.len() > 1 { "s" } else { "" },
        m.file().display()
    );
    for f in &facts {
        out.push_str(&format!("- {}{}\n", f.text, meta(f)));
    }
    out.push_str("\nIl modifie ce fichier à la main ou dans Réglages > Agent > Mémoire.");
    // Relus par l'agent : leur relecture par l'utilisateur attendra
    let texts: Vec<&str> = facts.iter().map(|f| f.text.as_str()).collect();
    let _ = m.touch(&texts);
    Ok(out)
}

/// La conversation de l'Assistant pendant laquelle l'agent parle, si c'est
/// lui qui a lancé odysseus-mcp
fn origin() -> Option<String> {
    let id: i64 = std::env::var("ODYSSEUS_CONVERSATION").ok()?.parse().ok()?;
    let title = Store::open().ok()?.title(id).ok()??;
    Some(if title.is_empty() {
        format!("conversation {id}")
    } else {
        format!("conversation « {title} »")
    })
}

/// Propose un fait ; gardé seulement si l'utilisateur l'accepte
pub fn remember(fact: &str, why: &str) -> Result<String, String> {
    let m = open()?;
    let t = memory::check(fact)?;
    if let Some(same) = m.find(&t) {
        return Err(format!("Déjà retenu : « {same} »"));
    }
    let mut body = format!("« {t} »");
    if !why.trim().is_empty() {
        body.push_str(&format!("\n\n{}", why.trim()));
    }
    body.push_str("\n\nGardé dans votre mémoire (Réglages > Agent) et lu à chaque conversation.");
    let key = notify_choice(
        "L'Assistant veut retenir",
        &body,
        &[("garder", "Garder"), ("refuser", "Refuser")],
        ASK_TIMEOUT,
    )?;
    if key.as_deref() != Some("garder") {
        return Err(
            "Refusé par l'utilisateur (ou pas de réponse) : n'insistez pas. Il peut l'écrire \
             lui-même dans Réglages > Agent > Mémoire s'il change d'avis."
                .into(),
        );
    }
    let kept = m.add(&t, origin().as_deref())?;
    Ok(format!(
        "Retenu : « {kept} ». L'utilisateur peut l'oublier dans Réglages > Agent > Mémoire."
    ))
}

/// Le fait désigné : le texte exact, ou un morceau qui n'en désigne qu'un
fn designated(m: &Memory, text: &str) -> Result<String, String> {
    if let Some(found) = m.find(text) {
        return Ok(found);
    }
    let key = common::text::fold(&memory::normalize(text));
    if key.is_empty() {
        return Err("Quel fait ? L'outil memoire les donne.".into());
    }
    let hits: Vec<String> = m
        .facts()
        .into_iter()
        .map(|f| f.text)
        .filter(|f| common::text::fold(f).contains(&key))
        .collect();
    match hits.len() {
        1 => Ok(hits.into_iter().next().unwrap_or_default()),
        0 => Err(format!(
            "Rien de tel dans la mémoire : « {} ». L'outil memoire la donne telle quelle.",
            memory::normalize(text)
        )),
        _ => Err(format!(
            "Plusieurs faits correspondent, précisez : {}",
            hits.iter()
                .map(|h| format!("« {h} »"))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// Propose d'oublier un fait ; retiré seulement si l'utilisateur l'accepte
pub fn forget(fact: &str) -> Result<String, String> {
    let m = open()?;
    let found = designated(&m, fact)?;
    let key = notify_choice(
        "L'Assistant veut oublier",
        &format!("« {found} »\n\nRetiré de votre mémoire si vous acceptez."),
        &[("oublier", "Oublier"), ("garder", "Garder")],
        ASK_TIMEOUT,
    )?;
    if key.as_deref() != Some("oublier") {
        return Err("Refusé par l'utilisateur (ou pas de réponse) : le fait reste.".into());
    }
    m.remove(&found)?;
    Ok(format!("Oublié : « {found} »"))
}

fn agent_label(agent: &str) -> &'static str {
    match agent {
        "ollama" => "Ollama",
        "commande" => "commande personnalisée",
        _ => "Claude Code",
    }
}

/// Les conversations précédentes de l'Assistant, ou l'une d'elles en entier
pub fn conversations(search: &str, id: Option<i64>) -> Result<String, String> {
    let store = Store::open()?;
    if let Some(id) = id {
        return read(&store, id);
    }
    let list = store.list(search)?;
    if list.is_empty() {
        return Ok(if search.trim().is_empty() {
            "Aucune conversation gardée pour l'instant.".into()
        } else {
            format!("Aucune conversation ne contient « {} ».", search.trim())
        });
    }
    let mut out = format!(
        "{} conversation{} (les plus récentes d'abord ; « id » pour en lire une) :\n",
        list.len(),
        if list.len() > 1 { "s" } else { "" }
    );
    for c in list.iter().take(MAX_LISTED) {
        out.push_str(&format!(
            "- id {} · {} · {} · {} question{} : « {} »\n",
            c.id,
            date(c.modified),
            agent_label(&c.agent),
            c.questions,
            if c.questions > 1 { "s" } else { "" },
            if c.title.is_empty() {
                "sans titre"
            } else {
                &c.title
            }
        ));
    }
    if list.len() > MAX_LISTED {
        out.push_str(&format!(
            "… et {} autres : « chercher » pour affiner.\n",
            list.len() - MAX_LISTED
        ));
    }
    Ok(out)
}

fn read(store: &Store, id: i64) -> Result<String, String> {
    let c = store
        .load(id)?
        .ok_or_else(|| format!("Pas de conversation {id} (l'outil conversations les liste)"))?;
    let title = store.title(id)?.unwrap_or_default();
    let mut out = format!(
        "Conversation {id}{} ({}) :\n\n",
        if title.is_empty() {
            String::new()
        } else {
            format!(" « {title} »")
        },
        agent_label(&c.agent)
    );
    for m in &c.messages {
        match m.kind {
            Kind::User => {
                out.push_str(&format!("Utilisateur : {}\n", m.body));
                let files: Vec<&str> = m.extra.lines().filter(|l| !l.is_empty()).collect();
                if !files.is_empty() {
                    out.push_str(&format!("(fichiers joints : {})\n", files.join(", ")));
                }
            }
            Kind::Answer => out.push_str(&format!("Assistant : {}\n", m.body)),
            Kind::Tool => out.push_str(&format!(
                "[outil : {}{}]\n",
                m.body,
                match m.extra.as_str() {
                    "refuse" => ", refusé",
                    "interrompu" => ", interrompu",
                    _ => "",
                }
            )),
            Kind::Error => out.push_str(&format!("[erreur : {}]\n", m.body)),
        }
        out.push('\n');
    }
    let total = out.chars().count();
    if total > MAX_READ {
        let head: String = out.chars().take(MAX_READ).collect();
        out = format!("{head}\n[… {} caractères omis …]", total - MAX_READ);
    }
    Ok(out)
}
