//! Les notes et leurs dossiers, dans une base SQLite
//! (~/.local/share/odysseus/notes.db).
//!
//! Une note supprimée va d'abord dans la corbeille ; elle y reste 30 jours.

use std::path::PathBuf;

use rusqlite::{Connection, OptionalExtension, params};

/// Durée de vie dans la corbeille
const TRASH_DAYS: i64 = 30;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum View {
    All,
    Folder(i64),
    Trash,
}

#[derive(Clone)]
pub struct Folder {
    pub id: i64,
    pub name: String,
    pub count: usize,
}

#[derive(Clone)]
pub struct Summary {
    pub id: i64,
    pub folder: Option<i64>,
    pub title: String,
    pub preview: String,
    pub modified: i64,
}

pub struct Store {
    db: Connection,
}

pub fn path() -> PathBuf {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into())).join(".local/share")
        });
    data.join("odysseus/notes.db")
}

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

fn err(e: rusqlite::Error) -> String {
    e.to_string()
}

impl Store {
    pub fn open() -> Result<Self, String> {
        let path = path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{} : {e}", dir.display()))?;
        }
        let db = Connection::open(&path).map_err(err)?;
        Self::init(db)
    }

    #[cfg(test)]
    fn memory() -> Self {
        Self::init(Connection::open_in_memory().unwrap()).unwrap()
    }

    fn init(db: Connection) -> Result<Self, String> {
        db.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA foreign_keys = ON;
             CREATE TABLE IF NOT EXISTS folders (
                 id INTEGER PRIMARY KEY,
                 name TEXT NOT NULL,
                 position INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE IF NOT EXISTS notes (
                 id INTEGER PRIMARY KEY,
                 folder INTEGER REFERENCES folders(id) ON DELETE SET NULL,
                 body TEXT NOT NULL DEFAULT '',
                 created INTEGER NOT NULL,
                 modified INTEGER NOT NULL,
                 deleted INTEGER
             );
             CREATE INDEX IF NOT EXISTS notes_modified ON notes(modified);",
        )
        .map_err(err)?;
        let store = Self { db };
        let empty: i64 = store
            .db
            .query_row("SELECT COUNT(*) FROM folders", [], |r| r.get(0))
            .map_err(err)?;
        if empty == 0 {
            store.create_folder("Notes")?;
        }
        // Corbeille : ce qui a plus de 30 jours part pour de bon
        store
            .db
            .execute(
                "DELETE FROM notes WHERE deleted IS NOT NULL AND deleted < ?1",
                [now() - TRASH_DAYS * 86_400],
            )
            .map_err(err)?;
        Ok(store)
    }

    pub fn folders(&self) -> Result<Vec<Folder>, String> {
        let mut q = self
            .db
            .prepare(
                "SELECT f.id, f.name, (SELECT COUNT(*) FROM notes n
                     WHERE n.folder = f.id AND n.deleted IS NULL)
                 FROM folders f ORDER BY f.position, f.id",
            )
            .map_err(err)?;
        q.query_map([], |r| {
            Ok(Folder {
                id: r.get(0)?,
                name: r.get(1)?,
                count: r.get::<_, i64>(2)? as usize,
            })
        })
        .map_err(err)?
        .collect::<Result<_, _>>()
        .map_err(err)
    }

    /// Nombre de notes : toutes, et dans la corbeille
    pub fn counts(&self) -> (usize, usize) {
        let n = |sql: &str| {
            self.db
                .query_row(sql, [], |r| r.get::<_, i64>(0))
                .unwrap_or(0) as usize
        };
        (
            n("SELECT COUNT(*) FROM notes WHERE deleted IS NULL"),
            n("SELECT COUNT(*) FROM notes WHERE deleted IS NOT NULL"),
        )
    }

    /// Notes d'une vue, les plus récentes d'abord ; `search` filtre sur le
    /// texte (sans tenir compte de la casse)
    pub fn notes(&self, view: View, search: &str) -> Result<Vec<Summary>, String> {
        let (sql, arg) = match view {
            View::All => ("deleted IS NULL", None),
            View::Folder(id) => ("deleted IS NULL AND folder = ?1", Some(id)),
            View::Trash => ("deleted IS NOT NULL", None),
        };
        let mut q = self
            .db
            .prepare(&format!(
                "SELECT id, folder, body, modified FROM notes WHERE {sql} ORDER BY modified DESC"
            ))
            .map_err(err)?;
        let words: Vec<String> = search.split_whitespace().map(fold).collect();
        let map = |r: &rusqlite::Row| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Option<i64>>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
            ))
        };
        let rows = match arg {
            Some(id) => q.query_map([id], map),
            None => q.query_map([], map),
        }
        .map_err(err)?;
        let mut out = Vec::new();
        for row in rows {
            let (id, folder, body, modified) = row.map_err(err)?;
            if !words.is_empty() {
                let hay = fold(&body);
                if !words.iter().all(|w| hay.contains(w.as_str())) {
                    continue;
                }
            }
            out.push(Summary {
                id,
                folder,
                title: crate::editor::title(&body),
                preview: crate::editor::preview(&body),
                modified,
            });
        }
        Ok(out)
    }

    pub fn body(&self, id: i64) -> Result<String, String> {
        self.db
            .query_row("SELECT body FROM notes WHERE id = ?1", [id], |r| r.get(0))
            .map_err(err)
    }

    pub fn modified(&self, id: i64) -> Option<i64> {
        self.db
            .query_row("SELECT modified FROM notes WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .optional()
            .ok()
            .flatten()
    }

    pub fn create_note(&self, folder: Option<i64>) -> Result<i64, String> {
        let t = now();
        self.db
            .execute(
                "INSERT INTO notes (folder, body, created, modified) VALUES (?1, '', ?2, ?2)",
                params![folder, t],
            )
            .map_err(err)?;
        Ok(self.db.last_insert_rowid())
    }

    pub fn save(&self, id: i64, body: &str) -> Result<(), String> {
        self.db
            .execute(
                "UPDATE notes SET body = ?1, modified = ?2 WHERE id = ?3",
                params![body, now(), id],
            )
            .map(|_| ())
            .map_err(err)
    }

    pub fn trash(&self, id: i64) -> Result<(), String> {
        self.db
            .execute(
                "UPDATE notes SET deleted = ?1 WHERE id = ?2",
                params![now(), id],
            )
            .map(|_| ())
            .map_err(err)
    }

    /// Sortie de la corbeille ; sans dossier (supprimé entre-temps), dans le premier
    pub fn restore(&self, id: i64) -> Result<(), String> {
        let first: Option<i64> = self
            .db
            .query_row(
                "SELECT id FROM folders ORDER BY position, id LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(err)?;
        self.db
            .execute(
                "UPDATE notes SET deleted = NULL, folder = COALESCE(folder, ?1) WHERE id = ?2",
                params![first, id],
            )
            .map(|_| ())
            .map_err(err)
    }

    /// Supprimée pour de bon
    pub fn purge(&self, id: i64) -> Result<(), String> {
        self.db
            .execute("DELETE FROM notes WHERE id = ?1", [id])
            .map(|_| ())
            .map_err(err)
    }

    pub fn empty_trash(&self) -> Result<usize, String> {
        self.db
            .execute("DELETE FROM notes WHERE deleted IS NOT NULL", [])
            .map_err(err)
    }

    pub fn move_note(&self, id: i64, folder: i64) -> Result<(), String> {
        self.db
            .execute(
                "UPDATE notes SET folder = ?1 WHERE id = ?2",
                params![folder, id],
            )
            .map(|_| ())
            .map_err(err)
    }

    pub fn create_folder(&self, name: &str) -> Result<i64, String> {
        self.db
            .execute(
                "INSERT INTO folders (name, position)
                 VALUES (?1, (SELECT COALESCE(MAX(position), 0) + 1 FROM folders))",
                [name],
            )
            .map_err(err)?;
        Ok(self.db.last_insert_rowid())
    }

    pub fn rename_folder(&self, id: i64, name: &str) -> Result<(), String> {
        self.db
            .execute(
                "UPDATE folders SET name = ?1 WHERE id = ?2",
                params![name, id],
            )
            .map(|_| ())
            .map_err(err)
    }

    /// Le dossier disparaît ; ses notes vont à la corbeille
    pub fn delete_folder(&self, id: i64) -> Result<(), String> {
        let tx = self.db.unchecked_transaction().map_err(err)?;
        tx.execute(
            "UPDATE notes SET deleted = ?1 WHERE folder = ?2 AND deleted IS NULL",
            params![now(), id],
        )
        .map_err(err)?;
        tx.execute("DELETE FROM folders WHERE id = ?1", [id])
            .map_err(err)?;
        tx.commit().map_err(err)
    }
}

/// Minuscules sans accents, pour chercher « ete » dans « Été »
pub fn fold(s: &str) -> String {
    s.chars()
        .flat_map(char::to_lowercase)
        .map(|c| match c {
            'à' | 'â' | 'ä' | 'á' | 'ã' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'î' | 'ï' | 'í' | 'ì' => 'i',
            'ô' | 'ö' | 'ó' | 'ò' | 'õ' => 'o',
            'ù' | 'û' | 'ü' | 'ú' => 'u',
            'ç' => 'c',
            'ÿ' => 'y',
            'ñ' => 'n',
            c => c,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle() {
        let s = Store::memory();
        let f = s.folders().unwrap();
        assert_eq!(f.len(), 1);
        let notes = f[0].id;
        let id = s.create_note(Some(notes)).unwrap();
        s.save(id, "Été au jardin\n- [ ] tomates").unwrap();
        assert_eq!(s.notes(View::All, "ete TOMATES").unwrap().len(), 1);
        assert_eq!(s.notes(View::All, "hiver").unwrap().len(), 0);
        let other = s.create_folder("Projets").unwrap();
        s.move_note(id, other).unwrap();
        assert_eq!(
            s.notes(View::Folder(other), "").unwrap()[0].title,
            "Été au jardin"
        );
        s.delete_folder(other).unwrap();
        assert_eq!(s.counts(), (0, 1));
        s.restore(id).unwrap();
        assert_eq!(s.notes(View::Folder(notes), "").unwrap().len(), 1);
        s.trash(id).unwrap();
        assert_eq!(s.empty_trash().unwrap(), 1);
    }
}
