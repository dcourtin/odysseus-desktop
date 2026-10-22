//! Client MCP minimal : lance odysseus-mcp et lui parle en JSON-RPC sur ses
//! entrée et sortie standard. Sert aux agents qui n'ont pas de client MCP à
//! eux (Ollama) : l'Assistant leur passe les outils du bureau.

use std::{
    io::{BufRead, BufReader, Write},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
};

use serde_json::{Value, json};

pub struct Mcp {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

/// Résultat d'un outil : texte, images (base64, JPEG), erreur signalée
pub struct ToolResult {
    pub text: String,
    pub images: Vec<String>,
    pub is_error: bool,
}

impl Mcp {
    /// `conversation` : celle du fil, pour que l'agent sache d'où vient ce qu'il retient
    pub fn start(conversation: Option<i64>) -> Result<Self, String> {
        let exe = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("odysseus-mcp")))
            .ok_or("odysseus-mcp introuvable")?;
        let mut cmd = Command::new(exe);
        if let Some(id) = conversation {
            cmd.env("ODYSSEUS_CONVERSATION", id.to_string());
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("odysseus-mcp : {e}"))?;
        let stdin = child.stdin.take().ok_or("entrée absente")?;
        let stdout = BufReader::new(child.stdout.take().ok_or("sortie absente")?);
        let mut mcp = Self {
            child,
            stdin,
            stdout,
            next_id: 1,
        };
        mcp.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "odysseus-assistant", "version": env!("CARGO_PKG_VERSION")}
            }),
        )?;
        mcp.notify("notifications/initialized")?;
        Ok(mcp)
    }

    fn notify(&mut self, method: &str) -> Result<(), String> {
        let msg = json!({"jsonrpc": "2.0", "method": method});
        writeln!(self.stdin, "{msg}").map_err(|e| e.to_string())
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(self.stdin, "{msg}").map_err(|e| e.to_string())?;
        self.stdin.flush().map_err(|e| e.to_string())?;
        let mut line = String::new();
        loop {
            line.clear();
            if self
                .stdout
                .read_line(&mut line)
                .map_err(|e| e.to_string())?
                == 0
            {
                return Err("odysseus-mcp s'est arrêté".into());
            }
            let Ok(reply) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if reply["id"] != json!(id) {
                continue;
            }
            if let Some(e) = reply.get("error") {
                return Err(e["message"].as_str().unwrap_or("erreur").to_string());
            }
            return Ok(reply["result"].clone());
        }
    }

    /// Outils au format d'Ollama (fonctions)
    pub fn tools_for_ollama(&mut self) -> Result<Vec<Value>, String> {
        let list = self.request("tools/list", json!({}))?;
        Ok(list["tools"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|t| {
                json!({"type": "function", "function": {
                    "name": t["name"],
                    "description": t["description"],
                    "parameters": t["inputSchema"],
                }})
            })
            .collect())
    }

    pub fn call(&mut self, name: &str, args: Value) -> Result<ToolResult, String> {
        let r = self.request("tools/call", json!({"name": name, "arguments": args}))?;
        let mut text = String::new();
        let mut images = Vec::new();
        for c in r["content"].as_array().into_iter().flatten() {
            match c["type"].as_str() {
                Some("text") => {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(c["text"].as_str().unwrap_or(""));
                }
                Some("image") => images.push(c["data"].as_str().unwrap_or("").to_string()),
                _ => {}
            }
        }
        Ok(ToolResult {
            text,
            images,
            is_error: r["isError"].as_bool().unwrap_or(false),
        })
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
