//! Outils d'odysseus-mcp : définition (nom, description, paramètres,
//! catégorie d'autorisation) et exécution. Les autorisations sont relues à
//! chaque appel : un changement dans Réglages > Agent vaut aussitôt.

use std::{
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use common::{ipc, settings::Settings, stats, theme};
use serde_json::{Value, json};

/// Catégories d'autorisation (Réglages > Agent)
#[derive(Clone, Copy, PartialEq)]
enum Cat {
    /// Toujours permis : dire ce qui est permis
    Always,
    Read,
    Desktop,
    Screen,
    Act,
    Clipboard,
}

impl Cat {
    fn label(self) -> &'static str {
        match self {
            Cat::Always => "toujours permis",
            Cat::Read => "Lire l'état du système",
            Cat::Desktop => "Voir le bureau",
            Cat::Screen => "Voir l'écran",
            Cat::Act => "Agir sur le bureau",
            Cat::Clipboard => "Presse-papiers",
        }
    }

    fn allowed(self, s: &Settings) -> bool {
        let p = s.agent_perms;
        match self {
            Cat::Always => true,
            Cat::Read => p.read,
            Cat::Desktop => p.desktop,
            Cat::Screen => p.screen,
            Cat::Act => p.act,
            Cat::Clipboard => p.clipboard,
        }
    }
}

struct Tool {
    name: &'static str,
    title: &'static str,
    cat: Cat,
    description: &'static str,
    schema: fn() -> Value,
}

fn no_args() -> Value {
    json!({"type": "object", "properties": {}})
}

const TOOLS: &[Tool] = &[
    Tool {
        name: "autorisations",
        title: "Autorisations",
        cat: Cat::Always,
        description: "Ce que l'utilisateur permet à l'agent sur ce bureau (Réglages > Agent), catégorie par catégorie, et les outils de chacune.",
        schema: no_args,
    },
    Tool {
        name: "etat_systeme",
        title: "État du système",
        cat: Cat::Read,
        description: "Mesure la machine pendant une seconde : charge du processeur (globale et par cœur), fréquence, température, mémoire, carte graphique (charge, mémoire vidéo, température, consommation, ventilateur), disques (place, débits), réseau, processus les plus actifs, durée depuis l'allumage.",
        schema: no_args,
    },
    Tool {
        name: "ecran",
        title: "Écran",
        cat: Cat::Read,
        description: "Écran branché : nom, définition et fréquence actuelles, définition recommandée, échelle, modes possibles, taille physique.",
        schema: no_args,
    },
    Tool {
        name: "reglages",
        title: "Réglages",
        cat: Cat::Read,
        description: "Réglages du bureau : thème, fond d'écran, échelle, lumière de nuit (mode, horaires, teinte), clavier, répétition des touches, extinction de l'écran, verrouillage, veille, agent choisi.",
        schema: no_args,
    },
    Tool {
        name: "journaux",
        title: "Journaux d'odysseus",
        cat: Cat::Read,
        description: "Dernières lignes du journal du compositeur ou du démon de notifications, pour diagnostiquer un problème du bureau.",
        schema: || {
            json!({"type": "object", "properties": {
                "source": {"type": "string", "enum": ["compositeur", "notifications"], "description": "Journal à lire (compositeur par défaut)"},
                "lignes": {"type": "integer", "minimum": 1, "maximum": 300, "description": "Nombre de lignes (60 par défaut)"}
            }})
        },
    },
    Tool {
        name: "fenetres",
        title: "Fenêtres",
        cat: Cat::Desktop,
        description: "Fenêtres ouvertes sur les neuf bureaux : identifiant, application, titre, bureau, premier plan, position et taille.",
        schema: no_args,
    },
    Tool {
        name: "notifications",
        title: "Notifications",
        cat: Cat::Desktop,
        description: "Notifications affichées et historique du centre de notifications : application, titre, texte, heure.",
        schema: no_args,
    },
    Tool {
        name: "capture_ecran",
        title: "Capture d'écran",
        cat: Cat::Screen,
        description: "Capture l'écran entier, ou une seule fenêtre (identifiant donné par « fenetres »), et la renvoie en image. L'utilisateur voit un témoin dans la barre pendant la capture.",
        schema: || {
            json!({"type": "object", "properties": {
                "fenetre": {"type": "integer", "description": "Identifiant de la fenêtre à capturer seule (écran entier si absent)"}
            }})
        },
    },
    Tool {
        name: "notifier",
        title: "Notifier",
        cat: Cat::Act,
        description: "Affiche une notification sur le bureau.",
        schema: || {
            json!({"type": "object", "properties": {
                "titre": {"type": "string"},
                "texte": {"type": "string"}
            }, "required": ["titre"]})
        },
    },
    Tool {
        name: "ouvrir",
        title: "Ouvrir",
        cat: Cat::Act,
        description: "Ouvre un fichier ou un dossier : dossiers dans Fichiers, images dans Aperçu, le reste avec l'application associée.",
        schema: || {
            json!({"type": "object", "properties": {
                "chemin": {"type": "string", "description": "Chemin absolu"}
            }, "required": ["chemin"]})
        },
    },
    Tool {
        name: "changer_bureau",
        title: "Changer de bureau",
        cat: Cat::Act,
        description: "Affiche un des neuf bureaux.",
        schema: || {
            json!({"type": "object", "properties": {
                "numero": {"type": "integer", "minimum": 1, "maximum": 9}
            }, "required": ["numero"]})
        },
    },
    Tool {
        name: "activer_fenetre",
        title: "Activer une fenêtre",
        cat: Cat::Act,
        description: "Met une fenêtre au premier plan (en allant sur son bureau si besoin).",
        schema: || {
            json!({"type": "object", "properties": {
                "id": {"type": "integer", "description": "Identifiant donné par « fenetres »"}
            }, "required": ["id"]})
        },
    },
    Tool {
        name: "regler",
        title: "Régler le bureau",
        cat: Cat::Act,
        description: "Change un ou plusieurs réglages : thème (bleue, sombre, claire), fond d'écran (« uni », nom d'un fond d'odysseus comme « Mer Égée », ou chemin d'une image), lumière de nuit (off, auto, on), échelle de l'écran (1 à 2).",
        schema: || {
            json!({"type": "object", "properties": {
                "theme": {"type": "string", "enum": ["bleue", "sombre", "claire"]},
                "fond": {"type": "string"},
                "lumiere_nuit": {"type": "string", "enum": ["off", "auto", "on"]},
                "echelle": {"type": "number", "minimum": 1, "maximum": 2}
            }})
        },
    },
    Tool {
        name: "lire_presse_papiers",
        title: "Lire le presse-papiers",
        cat: Cat::Clipboard,
        description: "Contenu du presse-papiers : texte, ou image.",
        schema: no_args,
    },
    Tool {
        name: "ecrire_presse_papiers",
        title: "Écrire dans le presse-papiers",
        cat: Cat::Clipboard,
        description: "Met du texte dans le presse-papiers.",
        schema: || {
            json!({"type": "object", "properties": {
                "texte": {"type": "string"}
            }, "required": ["texte"]})
        },
    },
];

pub fn list() -> Vec<Value> {
    TOOLS
        .iter()
        .map(|t| {
            let read_only = matches!(t.cat, Cat::Always | Cat::Read | Cat::Desktop | Cat::Screen)
                || t.name == "lire_presse_papiers";
            json!({
                "name": t.name,
                "title": t.title,
                "description": format!("{} (Catégorie : {}.)", t.description, t.cat.label()),
                "inputSchema": (t.schema)(),
                "annotations": {
                    "title": t.title,
                    "readOnlyHint": read_only,
                    "destructiveHint": false,
                    "openWorldHint": false
                }
            })
        })
        .collect()
}

/// Résultat d'un outil : contenu, ou erreur explicite (isError)
type Outcome = Result<Vec<Value>, String>;

fn text(t: impl Into<String>) -> Vec<Value> {
    vec![json!({"type": "text", "text": t.into()})]
}

pub fn call(name: &str, args: &Value) -> Value {
    let Some(tool) = TOOLS.iter().find(|t| t.name == name) else {
        return json!({"isError": true, "content": text(format!("Outil inconnu : {name}"))});
    };
    let settings = Settings::load();
    if !tool.cat.allowed(&settings) {
        log(name, false, "catégorie désactivée");
        return json!({"isError": true, "content": text(format!(
            "Refusé : la catégorie « {} » est désactivée dans Réglages > Agent. \
             L'utilisateur peut l'activer s'il le souhaite.",
            tool.cat.label()
        ))});
    }
    let outcome = run(name, args, &settings);
    log(name, true, &summary(args));
    match outcome {
        Ok(content) => json!({"content": content}),
        Err(e) => json!({"isError": true, "content": text(e)}),
    }
}

fn run(name: &str, args: &Value, settings: &Settings) -> Outcome {
    let s = |k: &str| args.get(k).and_then(Value::as_str);
    let n = |k: &str| args.get(k).and_then(Value::as_f64);
    match name {
        "autorisations" => Ok(text(permissions(settings))),
        "etat_systeme" => Ok(text(system_state())),
        "ecran" => screen(),
        "reglages" => Ok(text(settings_text(settings))),
        "journaux" => logs(
            s("source").unwrap_or("compositeur"),
            n("lignes").unwrap_or(60.0) as usize,
        ),
        "fenetres" => windows(),
        "notifications" => notifications(),
        "capture_ecran" => capture(n("fenetre").map(|v| v as u64)),
        "notifier" => notify(s("titre").unwrap_or(""), s("texte").unwrap_or("")),
        "ouvrir" => open(s("chemin").unwrap_or("")),
        "changer_bureau" => {
            let num = n("numero").unwrap_or(0.0) as u32;
            if !(1..=9).contains(&num) {
                return Err("Numéro de bureau entre 1 et 9".into());
            }
            ipc::command(&format!("workspace number {num}"))?;
            Ok(text(format!("Bureau {num} affiché")))
        }
        "activer_fenetre" => {
            let id = n("id").ok_or("Identifiant de fenêtre manquant")? as u64;
            ipc::command(&format!("focus {id}"))
                .map_err(|_| format!("Fenêtre {id} introuvable"))?;
            Ok(text(format!("Fenêtre {id} au premier plan")))
        }
        "regler" => adjust(args),
        "lire_presse_papiers" => clipboard_read(),
        "ecrire_presse_papiers" => {
            let t = s("texte").ok_or("Texte manquant")?;
            spawn_wait("wl-copy", &["--", t])?;
            Ok(text(format!("{} caractères copiés", t.chars().count())))
        }
        _ => Err(format!("Outil inconnu : {name}")),
    }
}

// --- Journal d'activité --------------------------------------------------------

fn log_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".local/state/odysseus/agent.log"))
}

/// Une ligne par appel : heure, accordé ou refusé, outil, paramètres résumés
fn log(tool: &str, granted: bool, detail: &str) {
    let Some(path) = log_path() else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // Le journal reste court : au-delà de 500 lignes, on garde les 300 dernières
    if let Ok(old) = std::fs::read_to_string(&path) {
        let lines: Vec<&str> = old.lines().collect();
        if lines.len() > 500 {
            let _ = std::fs::write(&path, lines[lines.len() - 300..].join("\n") + "\n");
        }
    }
    let line = format!(
        "{}\t{}\t{tool}\t{}\n",
        chrono::Local::now().format("%Y-%m-%dT%H:%M:%S"),
        if granted { "accordé" } else { "refusé" },
        detail.replace(['\t', '\n'], " ")
    );
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = f.write_all(line.as_bytes());
    }
}

fn summary(args: &Value) -> String {
    let Some(map) = args.as_object() else {
        return String::new();
    };
    map.iter()
        .map(|(k, v)| {
            let v = match v {
                Value::String(s) if s.chars().count() > 60 => {
                    format!("{}…", s.chars().take(60).collect::<String>())
                }
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            format!("{k}={v}")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

// --- Lecture --------------------------------------------------------------------

fn permissions(s: &Settings) -> String {
    let mut out = String::from("Autorisations de l'agent sur ce bureau (Réglages > Agent) :\n");
    for cat in [
        Cat::Read,
        Cat::Desktop,
        Cat::Screen,
        Cat::Act,
        Cat::Clipboard,
    ] {
        let tools: Vec<&str> = TOOLS
            .iter()
            .filter(|t| t.cat == cat)
            .map(|t| t.name)
            .collect();
        out.push_str(&format!(
            "- {} : {} ({})\n",
            cat.label(),
            if cat.allowed(s) { "permis" } else { "refusé" },
            tools.join(", ")
        ));
    }
    out
}

fn bytes(n: f64) -> String {
    const UNITS: [&str; 5] = ["o", "Ko", "Mo", "Go", "To"];
    let (mut v, mut u) = (n, 0);
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    let t = if u == 0 || v >= 100.0 {
        format!("{v:.0}")
    } else {
        format!("{v:.1}")
    };
    format!("{} {}", t.replace('.', ","), UNITS[u])
}

fn system_state() -> String {
    let mut sampler = stats::Sampler::new();
    std::thread::sleep(Duration::from_millis(800));
    let s = sampler.sample();
    let mut out = String::new();
    let c = &s.cpu;
    let mut cpu = format!(
        "Processeur : {} — {:.0} % ({} fils)",
        c.model,
        c.load * 100.0,
        c.cores.len()
    );
    if let Some(ghz) = c.ghz {
        cpu.push_str(&format!(", {:.2} GHz", ghz).replace('.', ","));
    }
    if let Some(t) = c.temp {
        cpu.push_str(&format!(", {t:.0} °C"));
    }
    out.push_str(&cpu);
    out.push_str(&format!(
        "\nCœurs : {}\n",
        c.cores
            .iter()
            .map(|v| format!("{:.0} %", v * 100.0))
            .collect::<Vec<_>>()
            .join(", ")
    ));
    let m = &s.memory;
    out.push_str(&format!(
        "Mémoire : {} utilisés sur {} (cache {})",
        bytes(m.used as f64),
        bytes(m.total as f64),
        bytes(m.cache as f64)
    ));
    if m.swap_total > 0 {
        out.push_str(&format!(
            ", échange {} sur {}",
            bytes(m.swap_used as f64),
            bytes(m.swap_total as f64)
        ));
    }
    out.push('\n');
    if let Some(g) = &s.gpu {
        let mut gpu = format!(
            "Carte graphique : {} — charge {:.0} %, mémoire vidéo {} sur {}",
            g.name,
            g.busy * 100.0,
            bytes(g.vram_used as f64),
            bytes(g.vram_total as f64)
        );
        if let Some(t) = g.temp {
            gpu.push_str(&format!(", {t:.0} °C"));
        }
        if let Some(w) = g.watts {
            gpu.push_str(&format!(", {w:.0} W"));
        }
        if let Some(rpm) = g.fan_rpm {
            gpu.push_str(&format!(", ventilateur {rpm} tr/min"));
        }
        out.push_str(&gpu);
        out.push('\n');
    }
    for d in &s.disks {
        out.push_str(&format!(
            "Disque {} ({}) : {} libres sur {}\n",
            d.label,
            d.mount.display(),
            bytes(d.free as f64),
            bytes(d.total as f64)
        ));
    }
    out.push_str(&format!(
        "Débit des disques : lecture {}/s, écriture {}/s\n",
        bytes(s.disk_read),
        bytes(s.disk_write)
    ));
    out.push_str(&format!(
        "Réseau ({}) : réception {}/s, envoi {}/s\n",
        s.net.interface,
        bytes(s.net.down),
        bytes(s.net.up)
    ));
    out.push_str("Processus les plus actifs (% d'un cœur, mémoire) :\n");
    for p in &s.processes {
        out.push_str(&format!(
            "- {} (pid {}) : {:.0} %, {}\n",
            p.name,
            p.pid,
            p.cpu,
            bytes(p.rss as f64)
        ));
    }
    let (d, h, mi) = (s.uptime / 86400, s.uptime / 3600 % 24, s.uptime / 60 % 60);
    out.push_str(&format!("Allumé depuis {d} j {h} h {mi} min"));
    out
}

fn screen() -> Outcome {
    let json: Value =
        serde_json::from_str(&ipc::request(ipc::GET_OUTPUTS, "")?).map_err(|e| e.to_string())?;
    let mut out = String::new();
    let mode = |v: &Value| {
        format!(
            "{} × {} à {} Hz",
            v["width"],
            v["height"],
            (v["refresh"].as_f64().unwrap_or(0.0) / 1000.0).round()
        )
    };
    for o in json.as_array().into_iter().flatten() {
        out.push_str(&format!(
            "Écran {} : {}, échelle {} (recommandé : {}), taille {} × {} mm\nModes : {}\n",
            o["name"].as_str().unwrap_or("?"),
            mode(&o["current_mode"]),
            o["scale"],
            mode(&o["preferred_mode"]),
            o["physical_width"],
            o["physical_height"],
            o["modes"]
                .as_array()
                .into_iter()
                .flatten()
                .map(&mode)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    Ok(text(if out.is_empty() {
        "Aucun écran".to_string()
    } else {
        out
    }))
}

fn settings_text(s: &Settings) -> String {
    let night = std::env::var_os("HOME")
        .and_then(|h| {
            std::fs::read_to_string(PathBuf::from(h).join(".local/state/odysseus/nightlight")).ok()
        })
        .map(|v| v.trim().to_string())
        .unwrap_or_else(|| "off".into());
    format!(
        "Thème : {}\nFond d'écran : {}\nÉchelle : {}\nMode de l'écran : {}\n\
         Lumière de nuit : {night} (de {} à {}, {} K)\nClavier : {} (répétition après {} ms, {} par seconde)\n\
         Extinction de l'écran : {}\nVerrouillage à l'extinction : {}\nVeille : {}\nAgent : {}",
        theme::palette().name,
        s.wallpaper
            .clone()
            .unwrap_or_else(|| "fond par défaut".into()),
        s.scale
            .map(|v| v.to_string())
            .unwrap_or_else(|| "par défaut".into()),
        s.mode.clone().unwrap_or_else(|| "recommandé".into()),
        s.night_from.clone().unwrap_or_else(|| "21:00".into()),
        s.night_to.clone().unwrap_or_else(|| "07:00".into()),
        s.night_temp.unwrap_or(3400),
        s.keyboard,
        s.repeat_delay,
        s.repeat_rate,
        if s.screen_off_min == 0 {
            "jamais".into()
        } else {
            format!("après {} min", s.screen_off_min)
        },
        if s.lock_on_idle { "oui" } else { "non" },
        if s.suspend_min == 0 {
            "jamais".into()
        } else {
            format!("après {} min", s.suspend_min)
        },
        s.agent
    )
}

fn logs(source: &str, lines: usize) -> Outcome {
    let file = match source {
        "notifications" => "notify.log",
        _ => "compositor.log",
    };
    let home = std::env::var_os("HOME").ok_or("HOME inconnu")?;
    let path = PathBuf::from(home).join(".local/state/odysseus").join(file);
    let content =
        std::fs::read_to_string(&path).map_err(|e| format!("{} : {e}", path.display()))?;
    let all: Vec<&str> = content.lines().collect();
    let n = lines.clamp(1, 300);
    let tail = all[all.len().saturating_sub(n)..].join("\n");
    Ok(text(strip_ansi(&tail)))
}

/// Retire les séquences de couleur du terminal
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            for d in chars.by_ref() {
                if d.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn windows() -> Outcome {
    let json: Value =
        serde_json::from_str(&ipc::request(ipc::GET_WINDOWS, "")?).map_err(|e| e.to_string())?;
    let list = json.as_array().cloned().unwrap_or_default();
    if list.is_empty() {
        return Ok(text("Aucune fenêtre ouverte"));
    }
    let lines: Vec<String> = list
        .iter()
        .map(|w| {
            format!(
                "- id {} : {} — « {} » (bureau {}{}), {} × {} en {}, {}",
                w["id"],
                w["app_id"].as_str().unwrap_or("?"),
                w["title"].as_str().unwrap_or(""),
                w["workspace"],
                if w["focused"].as_bool() == Some(true) {
                    ", au premier plan"
                } else {
                    ""
                },
                w["width"],
                w["height"],
                w["x"],
                w["y"]
            )
        })
        .collect();
    Ok(text(lines.join("\n")))
}

fn notifications() -> Outcome {
    use dbus::blocking::Connection;
    let conn = Connection::new_session().map_err(|e| e.to_string())?;
    let proxy = conn.with_proxy(
        "org.freedesktop.Notifications",
        "/org/freedesktop/Notifications",
        Duration::from_secs(3),
    );
    let (list,): (Vec<(String, String, String, String)>,) = proxy
        .method_call("fr.odysseus.Notifications", "GetHistory", ())
        .map_err(|e| format!("démon de notifications : {e}"))?;
    if list.is_empty() {
        return Ok(text("Aucune notification"));
    }
    Ok(text(
        list.iter()
            .map(|(app, title, body, time)| {
                let time = time.get(..16).unwrap_or(time).replace('T', " ");
                format!("- {time} · {app} : {title} — {body}")
            })
            .collect::<Vec<_>>()
            .join("\n"),
    ))
}

// --- Écran --------------------------------------------------------------------------

/// Témoin dans la barre : l'agent regarde l'écran (quelques secondes)
fn screen_witness() {
    let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") else {
        return;
    };
    let marker = PathBuf::from(dir).join("odysseus-agent-ecran");
    let _ = std::fs::write(&marker, "");
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(4));
        let _ = std::fs::remove_file(marker);
    });
}

fn capture(window: Option<u64>) -> Outcome {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").ok_or("XDG_RUNTIME_DIR absent")?;
    let path =
        PathBuf::from(dir).join(format!("odysseus-agent-capture-{}.png", std::process::id()));
    let _ = std::fs::remove_file(&path);
    screen_witness();
    let cmd = match window {
        Some(id) => format!("agent-capture {} {id}", path.display()),
        None => format!("agent-capture {}", path.display()),
    };
    ipc::command(&cmd)?;
    // Le compositeur l'écrit à l'image suivante
    let mut waited = 0;
    while !path.exists() && waited < 50 {
        std::thread::sleep(Duration::from_millis(100));
        waited += 1;
    }
    let png = std::fs::read(&path).map_err(|_| "Capture non reçue du compositeur".to_string())?;
    let _ = std::fs::remove_file(&path);
    let (data, w, h, sw, sh) = encode_for_agent(&png)?;
    Ok(vec![
        json!({"type": "image", "data": data, "mimeType": "image/jpeg"}),
        json!({"type": "text", "text": format!(
            "Capture {} ({w} × {h} pixels, réduite à {sw} × {sh})",
            if window.is_some() { "de la fenêtre" } else { "de l'écran" }
        )}),
    ])
}

/// Image réduite (1600 px de large au plus) en JPEG, encodée en base64
fn encode_for_agent(raw: &[u8]) -> Result<(String, u32, u32, u32, u32), String> {
    let img = image::load_from_memory(raw).map_err(|e| e.to_string())?;
    let (w, h) = (img.width(), img.height());
    let img = if w > 1600 {
        img.resize(
            1600,
            1600 * h / w.max(1),
            image::imageops::FilterType::Triangle,
        )
    } else {
        img
    };
    // Fond transparent (fenêtre seule) aplati sur du gris foncé
    let rgba = img.to_rgba8();
    let mut rgb = image::RgbImage::new(rgba.width(), rgba.height());
    for (x, y, p) in rgba.enumerate_pixels() {
        let a = p[3] as u32;
        let bg = 30u32;
        rgb.put_pixel(
            x,
            y,
            image::Rgb([0, 1, 2].map(|i| ((p[i] as u32 * a + bg * (255 - a)) / 255) as u8)),
        );
    }
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 85)
        .encode_image(&rgb)
        .map_err(|e| e.to_string())?;
    Ok((base64(&jpeg), w, h, rgb.width(), rgb.height()))
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

// --- Actions --------------------------------------------------------------------------

/// Programme d'odysseus installé à côté d'odysseus-mcp
fn sibling(name: &str) -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(name)))
        .unwrap_or_else(|| PathBuf::from(name))
}

fn spawn(program: impl AsRef<std::ffi::OsStr>, args: &[&str]) -> Result<(), String> {
    use std::os::unix::process::CommandExt;
    let mut child = Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|e| e.to_string())?;
    std::thread::spawn(move || child.wait());
    Ok(())
}

/// Lance et attend (commandes courtes) ; la sortie standard est jetée
fn spawn_wait(program: &str, args: &[&str]) -> Result<(), String> {
    let status = Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .status()
        .map_err(|e| format!("{program} : {e}"))?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| format!("{program} a échoué"))
}

fn notify(title: &str, body: &str) -> Outcome {
    if title.is_empty() {
        return Err("Titre manquant".into());
    }
    spawn_wait(
        &sibling("odysseus-notify").to_string_lossy(),
        &["--send", title, body],
    )?;
    Ok(text("Notification affichée"))
}

fn open(path: &str) -> Outcome {
    let p = Path::new(path);
    if !p.is_absolute() || !p.exists() {
        return Err(format!("Chemin introuvable (absolu attendu) : {path}"));
    }
    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if p.is_dir() {
        spawn(sibling("files"), &[path])?;
        return Ok(text(format!("Dossier ouvert dans Fichiers : {path}")));
    }
    if matches!(
        ext.as_str(),
        "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" | "tif" | "tiff"
    ) {
        spawn(sibling("apercu"), &[path])?;
        return Ok(text(format!("Image ouverte dans Aperçu : {path}")));
    }
    if spawn("xdg-open", &[path]).is_ok() {
        return Ok(text(format!("Ouvert avec l'application associée : {path}")));
    }
    // Sinon, son dossier dans Fichiers
    let parent = p
        .parent()
        .unwrap_or(Path::new("/"))
        .to_string_lossy()
        .into_owned();
    spawn(sibling("files"), &[&parent])?;
    Ok(text(format!(
        "Aucune application associée : dossier ouvert dans Fichiers ({parent})"
    )))
}

fn adjust(args: &Value) -> Outcome {
    let mut done = Vec::new();
    let mut settings = Settings::load();
    let mut save = false;
    if let Some(t) = args.get("theme").and_then(Value::as_str) {
        let id = theme::THEMES
            .iter()
            .find(|p| p.id == t || p.name.to_lowercase().ends_with(&t.to_lowercase()))
            .map(|p| p.id)
            .ok_or_else(|| format!("Thème inconnu : {t} (bleue, sombre, claire)"))?;
        theme::set(id).map_err(|e| e.to_string())?;
        done.push(format!("thème {}", theme::palette().name));
    }
    if let Some(f) = args.get("fond").and_then(Value::as_str) {
        let value = if f.eq_ignore_ascii_case("uni") {
            "uni".to_string()
        } else if Path::new(f).is_absolute() && Path::new(f).exists() {
            f.to_string()
        } else {
            // Un fond d'odysseus, par son nom
            let dir = std::env::var_os("HOME")
                .map(|h| PathBuf::from(h).join(".local/share/odysseus/fonds"))
                .ok_or("HOME inconnu")?;
            std::fs::read_dir(&dir)
                .map_err(|_| {
                    "Aucun fond d'odysseus (ouvrez Réglages > Fond d'écran une fois)".to_string()
                })?
                .flatten()
                .map(|e| e.path())
                .find(|p| {
                    p.file_stem()
                        .is_some_and(|s| s.to_string_lossy().to_lowercase() == f.to_lowercase())
                })
                .map(|p| p.to_string_lossy().into_owned())
                .ok_or_else(|| format!("Fond inconnu : {f}"))?
        };
        settings.wallpaper = Some(value);
        save = true;
        done.push(format!("fond d'écran « {f} »"));
    }
    if let Some(mode) = args.get("lumiere_nuit").and_then(Value::as_str) {
        let mode = match mode {
            "on" | "activée" | "activee" => "on",
            "auto" | "automatique" => "auto",
            _ => "off",
        };
        ipc::command(&format!("nightlight {mode}"))?;
        done.push(format!("lumière de nuit {mode}"));
    }
    if let Some(scale) = args.get("echelle").and_then(Value::as_f64) {
        if !(1.0..=2.0).contains(&scale) {
            return Err("Échelle entre 1 et 2".into());
        }
        ipc::command(&format!("output scale {scale}"))?;
        settings.scale = Some(scale);
        save = true;
        done.push(format!("échelle {scale}"));
    }
    if save {
        settings.save().map_err(|e| e.to_string())?;
    }
    if done.is_empty() {
        return Err("Rien à régler : theme, fond, lumiere_nuit ou echelle".into());
    }
    Ok(text(format!("Réglé : {}", done.join(", "))))
}

fn clipboard_read() -> Outcome {
    let types = Command::new("wl-paste")
        .arg("--list-types")
        .output()
        .map_err(|e| format!("wl-paste : {e}"))?;
    let types = String::from_utf8_lossy(&types.stdout).into_owned();
    if types.trim().is_empty() {
        return Ok(text("Le presse-papiers est vide"));
    }
    if let Some(mime) = types.lines().find(|t| t.starts_with("image/")) {
        let out = Command::new("wl-paste")
            .args(["--type", mime])
            .output()
            .map_err(|e| e.to_string())?;
        let (data, w, h, ..) = encode_for_agent(&out.stdout)?;
        return Ok(vec![
            json!({"type": "image", "data": data, "mimeType": "image/jpeg"}),
            json!({"type": "text", "text": format!("Image du presse-papiers ({w} × {h})")}),
        ]);
    }
    let out = Command::new("wl-paste")
        .arg("--no-newline")
        .output()
        .map_err(|e| e.to_string())?;
    let t = String::from_utf8_lossy(&out.stdout);
    let t: String = t.chars().take(20_000).collect();
    Ok(text(t))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn b64() {
        assert_eq!(base64(b"Man"), "TWFu");
        assert_eq!(base64(b"Ma"), "TWE=");
        assert_eq!(base64(b"M"), "TQ==");
    }

    #[test]
    fn ansi() {
        assert_eq!(strip_ansi("\u{1b}[2mheure\u{1b}[0m INFO"), "heure INFO");
    }

    #[test]
    fn tools_have_valid_names() {
        for t in TOOLS {
            assert!(t.name.chars().all(|c| c.is_ascii_lowercase() || c == '_'));
            assert!((t.schema)().is_object());
        }
    }
}
