//! Les agents de l'Assistant, chacun dans un fil, qui racontent leur réponse
//! au panneau par canal (texte au fil de l'eau, outils utilisés, fin) :
//! - Claude Code : `claude -p` en flux JSON, limité au serveur odysseus-mcp
//!   (et à la lecture des fichiers joints, à la recherche sur le web) ;
//!   la conversation continue par `--resume` ;
//! - Ollama : boucle maison sur /api/chat, avec les outils d'odysseus-mcp ;
//! - commande personnalisée : la question en argument, sa sortie affichée.

use std::{
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use common::agent::AgentConfig;
use serde_json::{Value, json};
use smithay_client_toolkit::reexports::calloop::channel::Sender;

use crate::mcp::Mcp;

const SYSTEM: &str = "Tu es l'Assistant du bureau odysseus-desktop, sur la machine de l'utilisateur \
(Linux From Scratch, bureau Wayland en Rust). Tu disposes des outils du bureau (serveur odysseus) : \
état du système, écran, fenêtres, notifications, réglages, captures, actions. Utilise-les quand la \
question porte sur la machine ou le bureau. Si un outil est refusé, explique que l'utilisateur peut \
l'autoriser dans Réglages > Agent. Réponds en français, de façon brève et claire, en texte simple \
(listes à puces permises, pas de tableaux).";

pub enum Event {
    /// Morceau de réponse
    Delta(String),
    /// Nouveau paragraphe (la réponse reprend après un outil)
    Break,
    /// Un outil commence (nom lisible), puis se termine (réussi ou refusé)
    Tool(String),
    ToolDone(bool),
    /// Conversation Claude Code, pour la suite
    Session(String),
    /// Historique Ollama, pour la suite
    History(Vec<Value>),
    Done(Result<(), String>),
}

/// Réponse en cours : de quoi l'interrompre
#[derive(Clone, Default)]
pub struct Job {
    cancel: Arc<AtomicBool>,
    pid: Arc<Mutex<Option<u32>>>,
}

impl Job {
    pub fn stop(&self) {
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(pid) = *self.pid.lock().unwrap() {
            unsafe { libc::kill(pid as i32, libc::SIGTERM) };
        }
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

/// Nom lisible d'un outil
pub fn tool_label(name: &str) -> String {
    let short = name.strip_prefix("mcp__odysseus__").unwrap_or(name);
    match short {
        "etat_systeme" => "Mesure du système",
        "ecran" => "Écran",
        "reglages" => "Lecture des réglages",
        "journaux" => "Lecture des journaux",
        "fenetres" => "Fenêtres ouvertes",
        "notifications" => "Notifications",
        "capture_ecran" => "Capture d'écran",
        "notifier" => "Notification",
        "ouvrir" => "Ouverture",
        "changer_bureau" => "Changement de bureau",
        "activer_fenetre" => "Fenêtre au premier plan",
        "regler" => "Réglage du bureau",
        "lire_presse_papiers" => "Lecture du presse-papiers",
        "ecrire_presse_papiers" => "Écriture du presse-papiers",
        "autorisations" => "Autorisations",
        "Read" => "Lecture d'un fichier",
        "WebSearch" => "Recherche sur le web",
        "WebFetch" => "Lecture d'une page web",
        other => return other.to_string(),
    }
    .to_string()
}

fn mcp_path() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("odysseus-mcp")))
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "odysseus-mcp".into())
}

fn prompt_with(question: &str, attachments: &[PathBuf]) -> String {
    if attachments.is_empty() {
        return question.to_string();
    }
    let list: Vec<String> = attachments
        .iter()
        .map(|p| p.display().to_string())
        .collect();
    format!(
        "Fichiers joints (chemins absolus) : {}\n\n{}",
        list.join(", "),
        if question.trim().is_empty() {
            "Que peux-tu me dire de ces fichiers ?"
        } else {
            question
        }
    )
}

/// Conversation en cours, à reprendre
#[derive(Clone, Default)]
pub struct Context {
    pub claude_session: Option<String>,
    pub ollama_history: Vec<Value>,
}

pub fn ask(
    agent: AgentConfig,
    question: String,
    attachments: Vec<PathBuf>,
    context: Context,
    tx: Sender<Event>,
) -> Job {
    let job = Job::default();
    let j = job.clone();
    std::thread::spawn(move || {
        let result = match agent.agent.as_str() {
            "ollama" => ollama(
                &agent,
                &question,
                &attachments,
                context.ollama_history,
                &tx,
                &j,
            ),
            "commande" => custom(&agent, &question, &attachments, &tx, &j),
            _ => claude(
                &agent,
                &question,
                &attachments,
                context.claude_session,
                &tx,
                &j,
            ),
        };
        let result = if j.cancelled() {
            Err("Interrompu".into())
        } else {
            result
        };
        let _ = tx.send(Event::Done(result));
    });
    job
}

// --- Claude Code ------------------------------------------------------------------

fn claude(
    agent: &AgentConfig,
    question: &str,
    attachments: &[PathBuf],
    session: Option<String>,
    tx: &Sender<Event>,
    job: &Job,
) -> Result<(), String> {
    let bin = agent.claude();
    if !bin.exists() {
        return Err("Claude Code n'est pas installé (Réglages > Agent)".into());
    }
    let mcp = json!({"mcpServers": {"odysseus": {"type": "stdio", "command": mcp_path()}}});
    let mut cmd = Command::new(bin);
    cmd.arg("-p")
        .arg(prompt_with(question, attachments))
        .args([
            "--output-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
        ])
        .args(["--strict-mcp-config", "--mcp-config"])
        .arg(mcp.to_string())
        .args([
            "--allowedTools",
            "mcp__odysseus",
            "Read",
            "WebSearch",
            "WebFetch",
        ])
        .arg("--append-system-prompt")
        .arg(SYSTEM);
    for p in attachments {
        if let Some(dir) = p.parent() {
            cmd.arg("--add-dir").arg(dir);
        }
    }
    if let Some(id) = session {
        cmd.args(["--resume", &id]);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
    let mut child = cmd
        .current_dir(home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Claude Code : {e}"))?;
    *job.pid.lock().unwrap() = Some(child.id());
    let stdout = child.stdout.take().ok_or("sortie absente")?;
    let mut error = None;
    let mut after_tool = false;
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        let Ok(e) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        match e["type"].as_str() {
            Some("system") => {
                if let Some(id) = e["session_id"].as_str() {
                    let _ = tx.send(Event::Session(id.to_string()));
                }
            }
            Some("stream_event") => {
                let ev = &e["event"];
                match ev["type"].as_str() {
                    Some("content_block_start") => {
                        let block = &ev["content_block"];
                        match block["type"].as_str() {
                            Some("tool_use") => {
                                let name = block["name"].as_str().unwrap_or("");
                                // La recherche d'outils de Claude Code ne regarde pas l'utilisateur
                                if name != "ToolSearch" {
                                    let _ = tx.send(Event::Tool(tool_label(name)));
                                    after_tool = true;
                                }
                            }
                            Some("text") if after_tool => {
                                let _ = tx.send(Event::Break);
                                after_tool = false;
                            }
                            _ => {}
                        }
                    }
                    Some("content_block_delta") => {
                        if let Some(t) = ev["delta"]["text"].as_str() {
                            let _ = tx.send(Event::Delta(t.to_string()));
                        }
                    }
                    _ => {}
                }
            }
            Some("user") => {
                for c in e["message"]["content"].as_array().into_iter().flatten() {
                    if c["type"] == "tool_result" {
                        // Résultat d'une recherche d'outils : rien à montrer
                        let is_search = c["content"]
                            .as_array()
                            .is_some_and(|a| a.iter().any(|x| x["type"] == "tool_reference"));
                        if !is_search {
                            let ok = !c["is_error"].as_bool().unwrap_or(false);
                            let _ = tx.send(Event::ToolDone(ok));
                        }
                    }
                }
            }
            Some("result") => {
                if let Some(id) = e["session_id"].as_str() {
                    let _ = tx.send(Event::Session(id.to_string()));
                }
                if e["is_error"].as_bool() == Some(true) {
                    error = Some(
                        e["result"]
                            .as_str()
                            .unwrap_or("Claude Code a signalé une erreur")
                            .to_string(),
                    );
                }
            }
            _ => {}
        }
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    *job.pid.lock().unwrap() = None;
    if let Some(e) = error {
        return Err(e);
    }
    if !status.success() && !job.cancelled() {
        let mut err = String::new();
        if let Some(mut s) = child.stderr.take() {
            use std::io::Read;
            let _ = s.read_to_string(&mut err);
        }
        let last = err
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("");
        return Err(if last.contains("login") || last.contains("auth") {
            "Claude Code n'est pas connecté : Réglages > Agent > Se connecter".into()
        } else if last.is_empty() {
            "Claude Code s'est arrêté".into()
        } else {
            last.to_string()
        });
    }
    Ok(())
}

// --- Ollama ---------------------------------------------------------------------

fn ollama(
    agent: &AgentConfig,
    question: &str,
    attachments: &[PathBuf],
    mut history: Vec<Value>,
    tx: &Sender<Event>,
    job: &Job,
) -> Result<(), String> {
    let mut mcp = Mcp::start()?;
    let tools = mcp.tools_for_ollama()?;
    history.push(json!({"role": "user", "content": prompt_with(question, attachments)}));
    // Quelques tours au plus : question, outils, réponse
    for _ in 0..8 {
        if job.cancelled() {
            break;
        }
        let mut messages = vec![json!({"role": "system", "content": SYSTEM})];
        messages.extend(history.iter().cloned());
        let mut body = json!({
            "model": agent.ollama_model,
            "messages": messages,
            "tools": tools,
            "stream": true,
        });
        // Pas de réflexion affichée : réponses plus rapides (les modèles
        // gpt-oss n'acceptent pas ce réglage)
        if !agent.ollama_model.starts_with("gpt-oss") {
            body["think"] = json!(false);
        }
        let mut child = Command::new("curl")
            .args(["-sN", "-X", "POST", "-d", "@-"])
            .arg(format!(
                "{}/api/chat",
                agent.ollama_url.trim_end_matches('/')
            ))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|e| format!("curl : {e}"))?;
        *job.pid.lock().unwrap() = Some(child.id());
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(body.to_string().as_bytes());
        }
        let stdout = child.stdout.take().ok_or("sortie absente")?;
        let mut content = String::new();
        let mut calls: Vec<Value> = Vec::new();
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if let Some(e) = v["error"].as_str() {
                let _ = child.wait();
                return Err(if e.contains("not found") {
                    format!(
                        "Le modèle {} n'est pas téléchargé (Réglages > Agent)",
                        agent.ollama_model
                    )
                } else {
                    e.to_string()
                });
            }
            if let Some(t) = v["message"]["content"].as_str()
                && !t.is_empty()
            {
                content.push_str(t);
                let _ = tx.send(Event::Delta(t.to_string()));
            }
            if let Some(c) = v["message"]["tool_calls"].as_array() {
                calls.extend(c.iter().cloned());
            }
        }
        let status = child.wait().map_err(|e| e.to_string())?;
        *job.pid.lock().unwrap() = None;
        if !status.success() && content.is_empty() && calls.is_empty() {
            return Err("Ollama ne répond pas (Réglages > Agent)".into());
        }
        let mut assistant = json!({"role": "assistant", "content": content});
        if !calls.is_empty() {
            assistant["tool_calls"] = Value::Array(calls.clone());
        }
        history.push(assistant);
        if calls.is_empty() {
            break;
        }
        // Outils demandés : exécutés par odysseus-mcp, résultats rendus au modèle
        for call in calls {
            let name = call["function"]["name"].as_str().unwrap_or("").to_string();
            let args = call["function"]["arguments"].clone();
            let _ = tx.send(Event::Tool(tool_label(&name)));
            let result = mcp.call(&name, if args.is_object() { args } else { json!({}) });
            let (text, ok, images) = match result {
                Ok(r) => (r.text, !r.is_error, r.images),
                Err(e) => (e, false, Vec::new()),
            };
            let _ = tx.send(Event::ToolDone(ok));
            let mut msg = json!({"role": "tool", "tool_name": name, "content": text});
            // Les modèles qui voient (Gemma 3…) reçoivent l'image ; les autres l'ignorent
            if !images.is_empty() {
                msg["images"] = json!(images);
            }
            history.push(msg);
        }
        let _ = tx.send(Event::Break);
    }
    let _ = tx.send(Event::History(history));
    Ok(())
}

// --- Commande personnalisée ---------------------------------------------------------

fn custom(
    agent: &AgentConfig,
    question: &str,
    attachments: &[PathBuf],
    tx: &Sender<Event>,
    job: &Job,
) -> Result<(), String> {
    if agent.custom_command.trim().is_empty() {
        return Err("Aucune commande définie (agent.json, clé commande.commande)".into());
    }
    // La question est le dernier argument de la commande
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(format!("{} \"$1\"", agent.custom_command))
        .arg("odysseus-assistant")
        .arg(prompt_with(question, attachments))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    *job.pid.lock().unwrap() = Some(child.id());
    let stdout = child.stdout.take().ok_or("sortie absente")?;
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        let _ = tx.send(Event::Delta(format!("{line}\n")));
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    if status.success() || job.cancelled() {
        Ok(())
    } else {
        Err(format!("la commande s'est terminée en erreur ({status})"))
    }
}
