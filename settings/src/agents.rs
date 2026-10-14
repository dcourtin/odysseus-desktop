//! Installation et état des agents, sans droits root :
//! - Ollama : archive officielle (GitHub, empreinte SHA-256 vérifiée) dans
//!   ~/.local/opt/ollama, service utilisateur systemd, modèles téléchargés par
//!   son API locale (127.0.0.1:11434) avec progression ;
//! - Claude Code : installeur officiel (claude.ai/install.sh) dans
//!   ~/.local/bin, mise à jour, connexion dans un terminal.
//!
//! Les tâches longues tournent dans un fil et donnent des nouvelles par canal.

use std::{
    io::{BufRead, BufReader},
    path::PathBuf,
    process::{Command, Stdio},
    time::Duration,
};

use smithay_client_toolkit::reexports::calloop::channel::Sender;

const API: &str = "http://127.0.0.1:11434";
const RELEASE: &str = "https://api.github.com/repos/ollama/ollama/releases/latest";
const ASSET: &str = "ollama-linux-amd64.tar.zst";

/// Tâche longue en cours
#[derive(Clone, PartialEq, Debug)]
pub enum Task {
    InstallOllama,
    Pull(String),
    InstallClaude,
    UpdateClaude,
}

/// Nouvelles d'une tâche : avancement (0–1, ou négatif si inconnu) et texte, puis fin
pub enum News {
    Step(Task, f32, String),
    Done(Task, Result<String, String>),
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
}

pub fn ollama_dir() -> PathBuf {
    home().join(".local/opt/ollama")
}

pub fn ollama_bin() -> Option<PathBuf> {
    let ours = ollama_dir().join("bin/ollama");
    if ours.exists() {
        return Some(ours);
    }
    std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .map(|d| PathBuf::from(d).join("ollama"))
        .find(|p| p.exists())
}

pub fn claude_bin() -> Option<PathBuf> {
    [
        home().join(".local/bin/claude"),
        PathBuf::from("/usr/local/bin/claude"),
    ]
    .into_iter()
    .find(|p| p.exists())
}

/// État des agents, relu à l'ouverture de la page et après chaque tâche
#[derive(Clone, Default)]
pub struct Status {
    pub ollama_installed: bool,
    pub ollama_running: bool,
    pub ollama_version: String,
    /// « carte graphique (Vulkan) », « processeur seul »… d'après le journal du service
    pub acceleration: Option<String>,
    /// Modèles présents : nom, taille en octets
    pub models: Vec<(String, u64)>,
    pub claude_version: Option<String>,
    pub claude_logged_in: bool,
}

/// Requête HTTP à l'API locale d'Ollama (curl : rien à embarquer)
fn api(method: &str, path: &str, body: Option<&str>) -> Option<String> {
    let mut cmd = Command::new("curl");
    cmd.args(["-sf", "-m", "3", "-X", method, &format!("{API}{path}")]);
    if let Some(b) = body {
        cmd.args(["-d", b]);
    }
    let out = cmd.output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

pub fn status() -> Status {
    let mut s = Status {
        ollama_installed: ollama_bin().is_some(),
        ..Default::default()
    };
    if let Some(v) = api("GET", "/api/version", None)
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
    {
        s.ollama_running = true;
        s.ollama_version = v["version"].as_str().unwrap_or("").to_string();
        if let Some(tags) = api("GET", "/api/tags", None)
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        {
            s.models = tags["models"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|m| Some((m["name"].as_str()?.to_string(), m["size"].as_u64()?)))
                .collect();
        }
        s.acceleration = acceleration();
    }
    if let Some(bin) = claude_bin() {
        s.claude_version = Command::new(bin).arg("--version").output().ok().map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_string()
        });
        s.claude_logged_in = home().join(".claude/.credentials.json").exists();
    }
    s
}

/// Ce qu'Ollama a trouvé pour calculer : lignes « inference compute » de son journal
fn acceleration() -> Option<String> {
    // Journal depuis le dernier démarrage du service : la détection y est au début
    let since = Command::new("systemctl")
        .args([
            "--user",
            "show",
            "ollama",
            "-p",
            "ActiveEnterTimestamp",
            "--value",
        ])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|t| !t.is_empty())?;
    let out = Command::new("journalctl")
        .args([
            "--user",
            "-u",
            "ollama",
            "--no-pager",
            "-o",
            "cat",
            "--since",
        ])
        .arg(&since)
        .output()
        .ok()?;
    let log = String::from_utf8_lossy(&out.stdout);
    let line = log
        .lines()
        .rev()
        .find(|l| l.contains("inference compute"))?;
    // Valeurs « clé=valeur » ou « clé="valeur avec espaces" »
    let field = |k: &str| {
        let start = line.find(&format!(" {k}="))? + k.len() + 2;
        let rest = &line[start..];
        Some(match rest.strip_prefix('"') {
            Some(quoted) => quoted.split('"').next().unwrap_or("").to_string(),
            None => rest.split_whitespace().next().unwrap_or("").to_string(),
        })
    };
    let library = field("library").unwrap_or_default();
    Some(match library.to_lowercase().as_str() {
        "cpu" | "" => "processeur seul".to_string(),
        lib => {
            // « AMD Radeon RX 9060 XT (RADV GFX1200) » → « AMD Radeon RX 9060 XT »
            let name = field("description")
                .or_else(|| field("name"))
                .map(|n| n.split(" (").next().unwrap_or(&n).to_string())
                .unwrap_or_default();
            let lib = match lib {
                "vulkan" => "Vulkan",
                "rocm" => "ROCm",
                "cuda" => "CUDA",
                other => other,
            };
            if name.is_empty() {
                format!("carte graphique ({lib})")
            } else {
                format!("{name} ({lib})")
            }
        }
    })
}

/// Lance une commande en arrière-plan du fil et renvoie son erreur lisible
fn run(cmd: &mut Command) -> Result<(), String> {
    let out = cmd.output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(err
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("échec")
            .trim()
            .to_string())
    }
}

// --- Ollama ------------------------------------------------------------------

pub fn install_ollama(tx: Sender<News>) {
    std::thread::spawn(move || {
        let task = Task::InstallOllama;
        let step = |f: f32, t: &str| {
            let _ = tx.send(News::Step(Task::InstallOllama, f, t.to_string()));
        };
        let result = (|| -> Result<String, String> {
            step(-1.0, "Recherche de la dernière version…");
            let release = Command::new("curl")
                .args(["-fsSL", "-m", "20", RELEASE])
                .output()
                .map_err(|e| e.to_string())?;
            let release: serde_json::Value = serde_json::from_slice(&release.stdout)
                .map_err(|_| "GitHub injoignable".to_string())?;
            let version = release["tag_name"].as_str().unwrap_or("?").to_string();
            let asset = release["assets"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|a| a["name"] == ASSET)
                .ok_or("archive introuvable dans la version publiée")?;
            let url = asset["browser_download_url"]
                .as_str()
                .ok_or("lien absent")?;
            let size = asset["size"].as_u64().unwrap_or(0);
            let digest = asset["digest"]
                .as_str()
                .and_then(|d| d.strip_prefix("sha256:"))
                .ok_or("empreinte SHA-256 absente : installation refusée")?
                .to_string();

            // Téléchargement, avancement d'après la taille du fichier partiel
            let base = home().join(".local/opt");
            std::fs::create_dir_all(&base).map_err(|e| e.to_string())?;
            let archive = base.join(format!(".{ASSET}.part"));
            let mut child = Command::new("curl")
                .args(["-fsL", "-o"])
                .arg(&archive)
                .arg(url)
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| e.to_string())?;
            loop {
                if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
                    if !status.success() {
                        let _ = std::fs::remove_file(&archive);
                        return Err("téléchargement interrompu".into());
                    }
                    break;
                }
                let got = std::fs::metadata(&archive).map(|m| m.len()).unwrap_or(0);
                if size > 0 {
                    step(
                        got as f32 / size as f32 * 0.85,
                        &format!(
                            "Téléchargement d'Ollama {version} : {} / {} Mo",
                            got / 1_000_000,
                            size / 1_000_000
                        ),
                    );
                }
                std::thread::sleep(Duration::from_millis(400));
            }

            step(0.87, "Vérification de l'empreinte SHA-256…");
            let sum = Command::new("sha256sum")
                .arg(&archive)
                .output()
                .map_err(|e| e.to_string())?;
            let sum = String::from_utf8_lossy(&sum.stdout);
            if sum.split_whitespace().next() != Some(digest.as_str()) {
                let _ = std::fs::remove_file(&archive);
                return Err("empreinte différente de celle publiée : archive supprimée".into());
            }

            step(0.9, "Décompression…");
            let fresh = base.join(".ollama.nouveau");
            let _ = std::fs::remove_dir_all(&fresh);
            std::fs::create_dir_all(&fresh).map_err(|e| e.to_string())?;
            run(Command::new("tar")
                .arg("--zstd")
                .arg("-xf")
                .arg(&archive)
                .arg("-C")
                .arg(&fresh))?;
            let _ = std::fs::remove_file(&archive);
            // Bibliothèques NVIDIA inutiles ici (carte AMD) : plusieurs Go de moins
            if let Ok(dir) = std::fs::read_dir(fresh.join("lib/ollama")) {
                for e in dir.flatten() {
                    let n = e.file_name().to_string_lossy().to_lowercase();
                    if n.starts_with("cuda") || n.contains("cublas") || n.contains("cudart") {
                        let p = e.path();
                        let _ = if p.is_dir() {
                            std::fs::remove_dir_all(&p)
                        } else {
                            std::fs::remove_file(&p)
                        };
                    }
                }
            }
            let target = ollama_dir();
            let _ = Command::new("systemctl")
                .args(["--user", "stop", "ollama"])
                .status();
            let _ = std::fs::remove_dir_all(&target);
            std::fs::rename(&fresh, &target).map_err(|e| e.to_string())?;
            // Commande « ollama » dans le terminal
            let link = home().join(".local/bin/ollama");
            let _ = std::fs::create_dir_all(home().join(".local/bin"));
            let _ = std::fs::remove_file(&link);
            let _ = std::os::unix::fs::symlink(target.join("bin/ollama"), &link);

            step(0.96, "Démarrage du service…");
            let unit = home().join(".config/systemd/user/ollama.service");
            std::fs::create_dir_all(unit.parent().unwrap()).map_err(|e| e.to_string())?;
            std::fs::write(
                &unit,
                "# odysseus-desktop : Ollama, modèles de langage locaux (Réglages > Agent)\n\
                 [Unit]\nDescription=Ollama (odysseus-desktop)\n\n\
                 [Service]\nExecStart=%h/.local/opt/ollama/bin/ollama serve\n\
                 Environment=OLLAMA_HOST=127.0.0.1:11434\nRestart=on-failure\nRestartSec=3\n\n\
                 [Install]\nWantedBy=default.target\n",
            )
            .map_err(|e| e.to_string())?;
            run(Command::new("systemctl").args(["--user", "daemon-reload"]))?;
            run(Command::new("systemctl").args(["--user", "enable", "--now", "ollama"]))?;
            for _ in 0..30 {
                if api("GET", "/api/version", None).is_some() {
                    return Ok(format!("Ollama {version} installé et démarré"));
                }
                std::thread::sleep(Duration::from_millis(500));
            }
            Err("installé, mais le service ne répond pas (journalctl --user -u ollama)".into())
        })();
        let _ = tx.send(News::Done(task, result));
    });
}

/// Télécharge un modèle par l'API d'Ollama (réponse en lignes JSON d'avancement)
pub fn pull(model: String, tx: Sender<News>) {
    std::thread::spawn(move || {
        let task = Task::Pull(model.clone());
        let result = (|| -> Result<String, String> {
            let mut child = Command::new("curl")
                .args(["-sN", "-X", "POST", &format!("{API}/api/pull"), "-d"])
                .arg(format!("{{\"model\":\"{model}\"}}"))
                .stdout(Stdio::piped())
                .spawn()
                .map_err(|e| e.to_string())?;
            let stdout = child.stdout.take().ok_or("sortie absente")?;
            let mut last_error = None;
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
                    continue;
                };
                if let Some(e) = v["error"].as_str() {
                    last_error = Some(e.to_string());
                    continue;
                }
                let status = v["status"].as_str().unwrap_or("");
                let (done, total) = (v["completed"].as_u64(), v["total"].as_u64());
                let (frac, text) = match (done, total) {
                    (Some(d), Some(t)) if t > 0 => (
                        d as f32 / t as f32,
                        format!("{} / {} Mo", d / 1_000_000, t / 1_000_000),
                    ),
                    _ => (
                        -1.0,
                        match status {
                            s if s.starts_with("pulling manifest") => "Préparation…".into(),
                            s if s.starts_with("verifying") => "Vérification…".into(),
                            s if s.starts_with("writing") => "Enregistrement…".into(),
                            "success" => "Terminé".into(),
                            s => s.to_string(),
                        },
                    ),
                };
                let _ = tx.send(News::Step(task.clone(), frac, text));
            }
            let _ = child.wait();
            match last_error {
                Some(e) => Err(e),
                None => Ok(format!("{model} téléchargé")),
            }
        })();
        let _ = tx.send(News::Done(task, result));
    });
}

pub fn delete(model: &str) -> Result<(), String> {
    api(
        "DELETE",
        "/api/delete",
        Some(&format!("{{\"model\":\"{model}\"}}")),
    )
    .map(|_| ())
    .ok_or_else(|| format!("{model} n'a pas pu être supprimé"))
}

// --- Claude Code ---------------------------------------------------------------

/// Installeur officiel (binaire natif dans ~/.local/bin), ou mise à jour
pub fn install_claude(update: bool, tx: Sender<News>) {
    std::thread::spawn(move || {
        let task = if update {
            Task::UpdateClaude
        } else {
            Task::InstallClaude
        };
        let _ = tx.send(News::Step(
            task.clone(),
            -1.0,
            if update {
                "Mise à jour de Claude Code…".into()
            } else {
                "Installation de Claude Code (installeur officiel)…".into()
            },
        ));
        let result = if update {
            match claude_bin() {
                Some(bin) => run(Command::new(bin).arg("update")),
                None => Err("Claude Code introuvable".into()),
            }
        } else {
            run(Command::new("bash")
                .arg("-c")
                .arg("set -o pipefail; curl -fsSL https://claude.ai/install.sh | bash"))
        }
        .map(|_| {
            if update {
                "Claude Code est à jour".into()
            } else {
                "Claude Code installé dans ~/.local/bin".into()
            }
        });
        let _ = tx.send(News::Done(task, result));
    });
}

/// Connexion à Claude Code : dans un terminal (navigateur et code à coller)
pub fn claude_login() -> Result<(), String> {
    use std::os::unix::process::CommandExt;
    let bin = claude_bin().ok_or("Claude Code introuvable")?;
    let term = std::env::var("TERMINAL").unwrap_or_else(|_| "foot".into());
    let mut child = Command::new(term)
        .arg("--title=Claude Code")
        .arg(bin)
        .current_dir(home())
        .process_group(0)
        .spawn()
        .map_err(|e| e.to_string())?;
    std::thread::spawn(move || child.wait());
    Ok(())
}
