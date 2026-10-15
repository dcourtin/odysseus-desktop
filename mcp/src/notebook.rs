//! Les notes de l'appli Notes, pour les agents : lister, lire, écrire,
//! mettre à la corbeille. Même base que l'appli
//! (~/.local/share/odysseus/notes.db), qui voit aussitôt les changements.

use ::notes::store::{Folder, Store, View, fold};

/// Au-delà, la liste demande d'affiner
const MAX_LISTED: usize = 150;

fn date(t: i64) -> String {
    chrono::DateTime::from_timestamp(t, 0)
        .map(|d| {
            d.with_timezone(&chrono::Local)
                .format("%d/%m/%Y %H:%M")
                .to_string()
        })
        .unwrap_or_default()
}

fn folder_name(folders: &[Folder], id: Option<i64>) -> String {
    id.and_then(|id| folders.iter().find(|f| f.id == id))
        .map_or_else(|| "sans dossier".into(), |f| format!("« {} »", f.name))
}

/// Dossier d'après son nom, sans tenir compte des accents ni de la casse
fn find<'a>(folders: &'a [Folder], name: &str) -> Option<&'a Folder> {
    let key = fold(name.trim());
    folders.iter().find(|f| fold(&f.name) == key)
}

fn names(folders: &[Folder]) -> String {
    folders
        .iter()
        .map(|f| format!("{} ({})", f.name, f.count))
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn list(folder: Option<&str>, search: &str, trash: bool) -> Result<String, String> {
    let store = Store::open()?;
    let folders = store.folders()?;
    let view = match folder.filter(|f| !f.trim().is_empty()) {
        _ if trash => View::Trash,
        Some(name) => View::Folder(
            find(&folders, name)
                .ok_or_else(|| {
                    format!("Pas de dossier « {name} ». Dossiers : {}", names(&folders))
                })?
                .id,
        ),
        None => View::All,
    };
    let notes = store.notes(view, search)?;
    let mut out = format!("Dossiers : {}\n\n", names(&folders));
    if notes.is_empty() {
        out.push_str("Aucune note ici.");
        return Ok(out);
    }
    for n in notes.iter().take(MAX_LISTED) {
        out.push_str(&format!(
            "- [{}] {} — {}, modifiée le {}\n",
            n.id,
            if n.title.is_empty() {
                "(sans titre)"
            } else {
                &n.title
            },
            folder_name(&folders, n.folder),
            date(n.modified),
        ));
        if !n.preview.is_empty() {
            let preview: String = n.preview.chars().take(140).collect();
            out.push_str(&format!("  {preview}\n"));
        }
    }
    if notes.len() > MAX_LISTED {
        out.push_str(&format!(
            "… et {} autres : affinez avec « chercher » ou « dossier ».\n",
            notes.len() - MAX_LISTED
        ));
    }
    Ok(out)
}

pub fn read(id: i64) -> Result<String, String> {
    let store = Store::open()?;
    let note = store.note(id)?.ok_or_else(|| format!("Pas de note {id}"))?;
    let folders = store.folders()?;
    Ok(format!(
        "Note {id}, dossier {}, créée le {}, modifiée le {}{}\n\n{}",
        folder_name(&folders, note.folder),
        date(note.created),
        date(note.modified),
        if note.deleted.is_some() {
            " — dans la corbeille"
        } else {
            ""
        },
        note.body
    ))
}

/// Crée une note (sans `id`) ou modifie la note `id` : texte remplacé ou
/// ajouté à la fin, note rangée dans `folder` (créé s'il n'existe pas)
pub fn write(
    id: Option<i64>,
    text: Option<&str>,
    folder: Option<&str>,
    append: bool,
) -> Result<String, String> {
    let store = Store::open()?;
    let folders = store.folders()?;
    let target = match folder.map(str::trim).filter(|f| !f.is_empty()) {
        Some(name) => Some(match find(&folders, name) {
            Some(f) => (f.id, f.name.clone()),
            None => (store.create_folder(name)?, name.to_string()),
        }),
        None => None,
    };
    let Some(id) = id else {
        let text = text
            .filter(|t| !t.trim().is_empty())
            .ok_or("Texte manquant pour une nouvelle note")?;
        let (fid, fname) = match target {
            Some(t) => t,
            None => folders
                .first()
                .map(|f| (f.id, f.name.clone()))
                .ok_or("Aucun dossier")?,
        };
        let id = store.create_note(Some(fid))?;
        store.save(id, text)?;
        return Ok(format!(
            "Note {id} créée dans « {fname} » : {}",
            ::notes::editor::title(text)
        ));
    };
    let note = store.note(id)?.ok_or_else(|| format!("Pas de note {id}"))?;
    if note.deleted.is_some() {
        return Err(format!(
            "La note {id} est dans la corbeille : l'utilisateur peut la remettre depuis Notes"
        ));
    }
    if text.is_none() && target.is_none() {
        return Err("Rien à faire : donnez « texte » ou « dossier »".into());
    }
    let mut done = Vec::new();
    if let Some(t) = text {
        let body = if append {
            let base = note.body.trim_end_matches('\n');
            if base.is_empty() {
                t.to_string()
            } else {
                format!("{base}\n{t}")
            }
        } else {
            t.to_string()
        };
        if body.trim().is_empty() {
            return Err("Texte vide : pour supprimer la note, utilisez supprimer_note".into());
        }
        store.save(id, &body)?;
        done.push(
            if append {
                "texte ajouté"
            } else {
                "texte remplacé"
            }
            .to_string(),
        );
    }
    if let Some((fid, fname)) = target {
        store.move_note(id, fid)?;
        done.push(format!("rangée dans « {fname} »"));
    }
    Ok(format!("Note {id} : {}", done.join(", ")))
}

pub fn trash(id: i64) -> Result<String, String> {
    let store = Store::open()?;
    let note = store.note(id)?.ok_or_else(|| format!("Pas de note {id}"))?;
    if note.deleted.is_some() {
        return Ok(format!("La note {id} est déjà dans la corbeille"));
    }
    store.trash(id)?;
    Ok(format!(
        "Note {id} « {} » mise à la corbeille (récupérable 30 jours depuis Notes)",
        ::notes::editor::title(&note.body)
    ))
}
