//! Exécution d'un automatisme validé : les étapes l'une après l'autre, avec
//! leurs variables ({resultat}, {fichier}, noms gardés…). Les actions sont
//! celles d'odysseus-mcp, sans demande d'accord : l'utilisateur les a
//! approuvées en validant l'automatisme.

use std::{
    collections::HashMap,
    io::Write,
    process::{Command, Stdio},
    time::Duration,
};

use common::{agent::AgentConfig, auto::Automatism};
use serde_json::{Value, json};

pub struct Ctx {
    pub name: String,
    pub vars: HashMap<String, String>,
}

impl Ctx {
    pub fn new(a: &Automatism, file: Option<&str>) -> Self {
        let now = chrono::Local::now();
        let mut vars = HashMap::new();
        vars.insert("date".into(), now.format("%Y-%m-%d").to_string());
        vars.insert("heure".into(), now.format("%H:%M").to_string());
        vars.insert("resultat".into(), String::new());
        if let Some(f) = file {
            vars.insert("fichier".into(), f.to_string());
            let name = std::path::Path::new(f)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            vars.insert("fichier_nom".into(), name);
        }
        Self {
            name: a.name.clone(),
            vars,
        }
    }

    /// Remplace {variable} dans un texte ; une variable inconnue reste telle quelle
    fn fill(&self, s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut rest = s;
        while let Some(open) = rest.find('{') {
            out.push_str(&rest[..open]);
            let after = &rest[open + 1..];
            match after.find('}') {
                Some(close)
                    if after[..close]
                        .chars()
                        .all(|c| c.is_alphanumeric() || c == '_')
                        && self.vars.contains_key(&after[..close]) =>
                {
                    out.push_str(&self.vars[&after[..close]]);
                    rest = &after[close + 1..];
                }
                _ => {
                    out.push('{');
                    rest = after;
                }
            }
        }
        out.push_str(rest);
        out
    }

    /// Dans une commande shell, une variable devient « ${ODYS_nom} », sa valeur
    /// passant par l'environnement : un nom de fichier piégé (« a\";rm -rf ~;\" »)
    /// n'est jamais interprété par le shell
    fn fill_command(&self, s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut rest = s;
        while let Some(open) = rest.find('{') {
            out.push_str(&rest[..open]);
            let after = &rest[open + 1..];
            match after.find('}') {
                Some(close)
                    if self.vars.contains_key(&after[..close]) && shell_name(&after[..close]) =>
                {
                    out.push_str(&format!("${{ODYS_{}}}", &after[..close]));
                    rest = &after[close + 1..];
                }
                _ => {
                    out.push('{');
                    rest = after;
                }
            }
        }
        out.push_str(rest);
        out
    }

    fn fill_value(&self, v: &Value) -> Value {
        match v {
            Value::String(s) => Value::String(self.fill(s)),
            Value::Array(a) => Value::Array(a.iter().map(|x| self.fill_value(x)).collect()),
            Value::Object(o) => Value::Object(
                o.iter()
                    .map(|(k, x)| (k.clone(), self.fill_value(x)))
                    .collect(),
            ),
            other => other.clone(),
        }
    }

    fn keep(&mut self, step: &Value, result: String) {
        if let Some(name) = step["garder"].as_str().filter(|n| !n.is_empty()) {
            self.vars.insert(name.to_string(), result.clone());
        }
        self.vars.insert("resultat".into(), result);
    }
}

/// Nom de variable utilisable tel quel par le shell
fn shell_name(k: &str) -> bool {
    !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn condition(cond: &Value, ctx: &Ctx) -> bool {
    let pair = |k: &str| {
        cond.get(k).and_then(Value::as_array).map(|v| {
            let f = |i: usize| ctx.fill(v.get(i).and_then(Value::as_str).unwrap_or(""));
            (f(0), f(1))
        })
    };
    if let Some((a, b)) = pair("contient") {
        return a.to_lowercase().contains(&b.to_lowercase());
    }
    if let Some((a, b)) = pair("egal") {
        return a.trim() == b.trim();
    }
    if let Some(v) = cond.get("vide").and_then(Value::as_str) {
        return ctx.fill(v).trim().is_empty();
    }
    if let Some(v) = cond.get("non_vide").and_then(Value::as_str) {
        return !ctx.fill(v).trim().is_empty();
    }
    false
}

/// Erreur d'une étape : « étape 3 (executer) : … »
pub fn run_steps(steps: &[Value], ctx: &mut Ctx, path: &str) -> Result<(), String> {
    for (i, step) in steps.iter().enumerate() {
        let here = format!("{path}{}", i + 1);
        if let Some(cond) = step.get("si") {
            let branch = if condition(cond, ctx) {
                "alors"
            } else {
                "sinon"
            };
            if let Some(list) = step.get(branch).and_then(Value::as_array) {
                run_steps(list, ctx, &format!("{here}.{branch}."))?;
            }
            continue;
        }
        let action = step["action"].as_str().unwrap_or("");
        let mut args = ctx.fill_value(step);
        if action == "executer" {
            let raw = step["commande"].as_str().unwrap_or("");
            args["commande"] = Value::String(ctx.fill_command(raw));
            // Le processus est seul (aucun autre fil) : on peut toucher à l'environnement
            for (k, v) in &ctx.vars {
                if shell_name(k) {
                    unsafe { std::env::set_var(format!("ODYS_{k}"), v) };
                }
            }
        }
        let result = match action {
            "attendre" => {
                let s = args["secondes"].as_f64().unwrap_or(1.0).clamp(0.0, 3600.0);
                std::thread::sleep(Duration::from_secs_f64(s));
                Ok(String::new())
            }
            "choisir" => {
                let choices: Vec<String> = args["choix"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|c| c.as_str().map(String::from))
                    .collect();
                let question = args["question"].as_str().unwrap_or("Que faire ?");
                match mcp::run::choose(&ctx.name, question, &choices)? {
                    Some(i) => Ok(choices[i].clone()),
                    None => return Err(format!("étape {here} : aucun choix, automatisme arrêté")),
                }
            }
            "agent" => agent(args["consigne"].as_str().unwrap_or("")),
            name => {
                let mut tool_args = args.clone();
                if let Some(o) = tool_args.as_object_mut() {
                    o.remove("action");
                    o.remove("garder");
                }
                mcp::tools::perform(name, &tool_args, &ctx.name)
            }
        }
        .map_err(|e| format!("étape {here} ({action}) : {e}"))?;
        ctx.keep(step, result);
    }
    Ok(())
}

/// Étape « agent » : le modèle choisi dans Réglages > Agent transforme un texte
fn agent(prompt: &str) -> Result<String, String> {
    if prompt.trim().is_empty() {
        return Err("consigne vide".into());
    }
    let cfg = AgentConfig::load();
    let output = match cfg.agent.as_str() {
        "ollama" => {
            let mut body = json!({
                "model": cfg.ollama_model,
                "messages": [
                    {"role": "system", "content": "Réponds en français, directement, sans préambule."},
                    {"role": "user", "content": prompt}
                ],
                "stream": false,
                "options": {"num_ctx": cfg.ollama_ctx},
            });
            if !cfg.ollama_model.starts_with("gpt-oss") {
                body["think"] = json!(false);
            }
            let mut child = Command::new("curl")
                .args(["-s", "--max-time", "300", "-X", "POST", "-d", "@-"])
                .arg(format!("{}/api/chat", cfg.ollama_url.trim_end_matches('/')))
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .map_err(|e| format!("curl : {e}"))?;
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(body.to_string().as_bytes());
            }
            let out = child.wait_with_output().map_err(|e| e.to_string())?;
            let v: Value = serde_json::from_slice(&out.stdout)
                .map_err(|_| "Ollama ne répond pas (Réglages > Agent)".to_string())?;
            if let Some(e) = v["error"].as_str() {
                return Err(format!("Ollama : {e}"));
            }
            v["message"]["content"].as_str().unwrap_or("").to_string()
        }
        "commande" => {
            let out = Command::new("/bin/sh")
                .arg("-c")
                .arg(format!("{} \"$1\"", cfg.custom_command))
                .arg("agent")
                .arg(prompt)
                .stdin(Stdio::null())
                .output()
                .map_err(|e| e.to_string())?;
            String::from_utf8_lossy(&out.stdout).into_owned()
        }
        _ => {
            let out = Command::new(cfg.claude())
                .args(["-p", "--output-format", "text"])
                .arg(prompt)
                .stdin(Stdio::null())
                .output()
                .map_err(|e| format!("Claude Code : {e}"))?;
            if !out.status.success() {
                return Err(format!(
                    "Claude Code : {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
            }
            String::from_utf8_lossy(&out.stdout).into_owned()
        }
    };
    Ok(output.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> Ctx {
        let mut vars = HashMap::new();
        vars.insert("fichier".to_string(), "/tmp/a.pdf".to_string());
        vars.insert("resultat".to_string(), "ok".to_string());
        Ctx {
            name: "essai".into(),
            vars,
        }
    }

    #[test]
    fn fills_known_variables_only() {
        let c = ctx();
        assert_eq!(c.fill("{fichier} → {resultat}"), "/tmp/a.pdf → ok");
        assert_eq!(
            c.fill("awk '{print $1}' {inconnu}"),
            "awk '{print $1}' {inconnu}"
        );
        let v = c.fill_value(&json!({"t": ["{resultat}", 3]}));
        assert_eq!(v, json!({"t": ["ok", 3]}));
    }

    #[test]
    fn commands_get_variables_through_the_environment() {
        let c = ctx();
        assert_eq!(
            c.fill_command("mv \"{fichier}\" /x && echo {inconnu}"),
            "mv \"${ODYS_fichier}\" /x && echo {inconnu}"
        );
    }

    #[test]
    fn conditions() {
        let c = ctx();
        assert!(condition(&json!({"contient": ["{fichier}", ".PDF"]}), &c));
        assert!(!condition(&json!({"egal": ["{resultat}", "non"]}), &c));
        assert!(condition(&json!({"vide": "{absent_mais_vide}x"}), &c) == false);
        assert!(condition(&json!({"non_vide": "{resultat}"}), &c));
    }

    #[test]
    fn steps_keep_results_and_branch() {
        let mut c = ctx();
        let steps = vec![
            json!({"action": "attendre", "secondes": 0, "garder": "rien"}),
            json!({"si": {"contient": ["{fichier}", "pdf"]}, "alors": [
                {"action": "attendre", "secondes": 0, "garder": "branche"}
            ]}),
        ];
        run_steps(&steps, &mut c, "").unwrap();
        assert!(c.vars.contains_key("rien") && c.vars.contains_key("branche"));
    }
}
