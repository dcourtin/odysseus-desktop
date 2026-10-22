//! Le fil de l'Assistant : les conversations, gardées dans une base SQLite
//! (~/.local/share/odysseus/assistant.db) pour être reprises plus tard, avec
//! ce qu'il faut au modèle pour continuer (session Claude Code, historique
//! Ollama).
//!
//! Rien n'en est lu d'office par l'agent : ce que l'Assistant lui donne à
//! chaque question reste la conversation en cours.

use std::path::PathBuf;

use common::text::fold;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;

/// Longueur du titre d'une conversation (sa première question)
const TITLE_CHARS: usize = 80;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    User,
    Answer,
    Tool,
    Error,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::User => "user",
            Kind::Answer => "answer",
            Kind::Tool => "tool",
            Kind::Error => "error",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "user" => Kind::User,
            "answer" => Kind::Answer,
            "tool" => Kind::Tool,
            "error" => Kind::Error,
            _ => return None,
        })
    }
}

/// Un élément d'une conversation. `extra` : pour une question, les noms des
/// fichiers joints (un par ligne) ; pour un outil, « ok » ou « refuse ».
#[derive(Clone, PartialEq, Debug)]
pub struct Message {
    pub kind: Kind,
    pub body: String,
    pub extra: String,
}

/// Une conversation dans la liste
#[derive(Clone, PartialEq, Debug)]
pub struct Summary {
    pub id: i64,
    pub title: String,
    /// Agent qui l'a menée : claude, ollama ou commande
    pub agent: String,
    pub modified: i64,
    /// Nombre de questions posées
    pub questions: usize,
}

/// Une conversation entière, de quoi la reprendre
#[derive(Clone, Default, Debug)]
pub struct Conversation {
    pub agent: String,
    pub claude_session: Option<String>,
    pub ollama_history: Vec<Value>,
    pub messages: Vec<Message>,
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
    data.join("odysseus/assistant.db")
}

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

fn err(e: rusqlite::Error) -> String {
    e.to_string()
}

/// Titre d'une conversation : sa première question, sur une ligne ; à défaut,
/// les fichiers joints
pub fn title(messages: &[Message]) -> String {
    let Some(first) = messages.iter().find(|m| m.kind == Kind::User) else {
        return String::new();
    };
    let line = first
        .body
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let text = if line.is_empty() {
        let files: Vec<&str> = first
            .extra
            .lines()
            .filter(|l| !l.is_empty() && !l.starts_with("contexte: "))
            .collect();
        let context = first
            .extra
            .lines()
            .find_map(|l| l.strip_prefix("contexte: "));
        match (files.is_empty(), context) {
            (false, _) => format!("Fichiers : {}", files.join(", ")),
            (true, Some(c)) => format!("Contexte : {c}"),
            (true, None) => String::new(),
        }
    } else {
        line.to_string()
    };
    if text.chars().count() > TITLE_CHARS {
        let cut: String = text.chars().take(TITLE_CHARS - 1).collect();
        format!("{}…", cut.trim_end())
    } else {
        text
    }
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
             CREATE TABLE IF NOT EXISTS conversations (
                 id INTEGER PRIMARY KEY,
                 title TEXT NOT NULL DEFAULT '',
                 agent TEXT NOT NULL DEFAULT '',
                 claude_session TEXT,
                 ollama_history TEXT,
                 created INTEGER NOT NULL,
                 modified INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS messages (
                 id INTEGER PRIMARY KEY,
                 conversation INTEGER NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
                 position INTEGER NOT NULL,
                 kind TEXT NOT NULL,
                 body TEXT NOT NULL DEFAULT '',
                 extra TEXT NOT NULL DEFAULT ''
             );
             CREATE INDEX IF NOT EXISTS messages_conversation ON messages(conversation, position);
             CREATE INDEX IF NOT EXISTS conversations_modified ON conversations(modified);",
        )
        .map_err(err)?;
        // Une conversation sans message (l'Assistant fermé avant la première
        // question) n'a rien à reprendre
        db.execute(
            "DELETE FROM conversations
             WHERE NOT EXISTS (SELECT 1 FROM messages m WHERE m.conversation = conversations.id)",
            [],
        )
        .map_err(err)?;
        Ok(Self { db })
    }

    /// Les conversations, la plus récente d'abord ; `search` filtre sur le
    /// titre et le texte des échanges (sans casse ni accents)
    pub fn list(&self, search: &str) -> Result<Vec<Summary>, String> {
        let words: Vec<String> = search.split_whitespace().map(fold).collect();
        let mut q = self
            .db
            .prepare(
                "SELECT c.id, c.title, c.agent, c.modified,
                        (SELECT COUNT(*) FROM messages m
                         WHERE m.conversation = c.id AND m.kind = 'user'),
                        (SELECT group_concat(m.body, ' ') FROM messages m
                         WHERE m.conversation = c.id AND m.kind IN ('user', 'answer'))
                 FROM conversations c ORDER BY c.modified DESC",
            )
            .map_err(err)?;
        let rows = q
            .query_map([], |r| {
                Ok((
                    Summary {
                        id: r.get(0)?,
                        title: r.get(1)?,
                        agent: r.get(2)?,
                        modified: r.get(3)?,
                        questions: r.get::<_, i64>(4)? as usize,
                    },
                    r.get::<_, Option<String>>(5)?.unwrap_or_default(),
                ))
            })
            .map_err(err)?;
        let mut out = Vec::new();
        for row in rows {
            let (summary, text) = row.map_err(err)?;
            if !words.is_empty() {
                let hay = fold(&format!("{} {text}", summary.title));
                if !words.iter().all(|w| hay.contains(w.as_str())) {
                    continue;
                }
            }
            out.push(summary);
        }
        Ok(out)
    }

    /// Titre d'une conversation ; None si elle n'existe pas
    pub fn title(&self, id: i64) -> Result<Option<String>, String> {
        self.db
            .query_row("SELECT title FROM conversations WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .optional()
            .map_err(err)
    }

    pub fn count(&self) -> usize {
        self.db
            .query_row("SELECT COUNT(*) FROM conversations", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap_or(0) as usize
    }

    pub fn load(&self, id: i64) -> Result<Option<Conversation>, String> {
        let head = self
            .db
            .query_row(
                "SELECT agent, claude_session, ollama_history FROM conversations WHERE id = ?1",
                [id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(err)?;
        let Some((agent, claude_session, history)) = head else {
            return Ok(None);
        };
        let ollama_history = history
            .and_then(|h| serde_json::from_str::<Value>(&h).ok())
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default();
        let mut q = self
            .db
            .prepare(
                "SELECT kind, body, extra FROM messages WHERE conversation = ?1 ORDER BY position",
            )
            .map_err(err)?;
        let messages = q
            .query_map([id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .map_err(err)?
            .filter_map(Result::ok)
            .filter_map(|(kind, body, extra)| {
                Some(Message {
                    kind: Kind::parse(&kind)?,
                    body,
                    extra,
                })
            })
            .collect();
        Ok(Some(Conversation {
            agent,
            claude_session,
            ollama_history,
            messages,
        }))
    }

    /// Nouvelle conversation, vide ; `save` la remplit
    pub fn create(&self, agent: &str) -> Result<i64, String> {
        let t = now();
        self.db
            .execute(
                "INSERT INTO conversations (agent, created, modified) VALUES (?1, ?2, ?2)",
                params![agent, t],
            )
            .map_err(err)?;
        Ok(self.db.last_insert_rowid())
    }

    /// La conversation entière remplace ce qui était gardé (quelques dizaines
    /// de lignes au plus : plus simple que de suivre ce qui a changé). Les
    /// images de l'historique Ollama (captures en base64) ne sont pas gardées.
    pub fn save(&self, id: i64, c: &Conversation) -> Result<(), String> {
        let history: Vec<Value> = c
            .ollama_history
            .iter()
            .map(|m| {
                let mut m = m.clone();
                if let Some(o) = m.as_object_mut() {
                    o.remove("images");
                }
                m
            })
            .collect();
        let history = if history.is_empty() {
            None
        } else {
            Some(Value::Array(history).to_string())
        };
        let tx = self.db.unchecked_transaction().map_err(err)?;
        tx.execute(
            "UPDATE conversations
             SET title = ?1, agent = ?2, claude_session = ?3, ollama_history = ?4, modified = ?5
             WHERE id = ?6",
            params![
                title(&c.messages),
                c.agent,
                c.claude_session,
                history,
                now(),
                id
            ],
        )
        .map_err(err)?;
        tx.execute("DELETE FROM messages WHERE conversation = ?1", [id])
            .map_err(err)?;
        {
            let mut insert = tx
                .prepare(
                    "INSERT INTO messages (conversation, position, kind, body, extra)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                )
                .map_err(err)?;
            for (i, m) in c.messages.iter().enumerate() {
                insert
                    .execute(params![id, i as i64, m.kind.as_str(), m.body, m.extra])
                    .map_err(err)?;
            }
        }
        tx.commit().map_err(err)
    }

    /// Supprimée pour de bon, avec ses messages
    pub fn delete(&self, id: i64) -> Result<(), String> {
        self.db
            .execute("DELETE FROM conversations WHERE id = ?1", [id])
            .map(|_| ())
            .map_err(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn msg(kind: Kind, body: &str) -> Message {
        Message {
            kind,
            body: body.into(),
            extra: String::new(),
        }
    }

    #[test]
    fn lifecycle() {
        let s = Store::memory();
        let id = s.create("ollama").unwrap();
        // Vide : rien dans la liste tant qu'on n'a rien dit… mais elle existe
        assert_eq!(s.list("").unwrap().len(), 1);
        let c = Conversation {
            agent: "ollama".into(),
            claude_session: None,
            ollama_history: vec![
                json!({"role": "user", "content": "Été au jardin ?"}),
                json!({"role": "tool", "content": "…", "images": ["AAAA"]}),
            ],
            messages: vec![
                msg(Kind::User, "Été au jardin ?"),
                msg(Kind::Tool, "Mesure du système"),
                msg(Kind::Answer, "Il fait 31 °C."),
            ],
        };
        s.save(id, &c).unwrap();
        let list = s.list("").unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].title, "Été au jardin ?");
        assert_eq!(list[0].questions, 1);
        assert_eq!(s.list("ete 31").unwrap().len(), 1);
        assert_eq!(s.list("hiver").unwrap().len(), 0);
        let back = s.load(id).unwrap().unwrap();
        assert_eq!(back.messages, c.messages);
        assert_eq!(back.agent, "ollama");
        // Les images ne sont pas gardées, le reste si
        assert_eq!(back.ollama_history.len(), 2);
        assert!(back.ollama_history[1].get("images").is_none());
        assert_eq!(back.ollama_history[0]["content"], "Été au jardin ?");
        s.delete(id).unwrap();
        assert!(s.load(id).unwrap().is_none());
        assert_eq!(s.count(), 0);
    }

    #[test]
    fn titles() {
        assert_eq!(title(&[]), "");
        let files = Message {
            kind: Kind::User,
            body: String::new(),
            extra: "a.png\nb.pdf".into(),
        };
        assert_eq!(title(&[files]), "Fichiers : a.png, b.pdf");
        let context = Message {
            kind: Kind::User,
            body: String::new(),
            extra: "contexte: fenêtre, sélection".into(),
        };
        assert_eq!(title(&[context]), "Contexte : fenêtre, sélection");
        let long = "x".repeat(100);
        let t = title(&[msg(Kind::User, &format!("\n  {long}"))]);
        assert_eq!(t.chars().count(), TITLE_CHARS);
        assert!(t.ends_with('…'));
    }

    #[test]
    fn empty_conversations_are_dropped_at_open() {
        let s = Store::memory();
        s.create("claude").unwrap();
        assert_eq!(s.count(), 1);
        // Même base rouverte : la conversation vide disparaît
        let again = Store::init(s.db).unwrap();
        assert_eq!(again.count(), 0);
    }
}
