//! odysseus-veille : cherche les mises à jour, sans rien installer. La
//! décision revient à l'utilisateur, au cas par cas (Réglages > Mises à jour,
//! ouverte par l'icône qui apparaît dans la barre).
//!
//! - **Système** : chaque archive de /sources est un logiciel construit ici ;
//!   sa version est comparée à la dernière version stable connue
//!   (release-monitoring.org, correspondances d'Arch Linux), le noyau à
//!   kernel.org, Firefox ESR à Mozilla. Un avis de sécurité LFS/BLFS paru
//!   après l'archive installée la signale en priorité.
//! - **odysseus** : nouveaux commits sur le dépôt distant.
//! - **Outils** : chaîne Rust (rustup).
//!
//! Le rapport va dans ~/.local/state/odysseus/mises-a-jour.json ; une
//! notification annonce ce qui est nouveau depuis la vérification précédente.
//!
//! `odysseus-veille [verifier]` · `odysseus-veille ignorer NOM VERSION` ·
//! `odysseus-veille etat`

mod version;

use std::{
    collections::{BTreeMap, HashSet},
    path::PathBuf,
    process::Command,
    sync::Mutex,
};

use std::os::unix::fs::MetadataExt;

use common::updates::{self, Advisory, Kind, Level, Report, Update};
use serde_json::Value;

/// Dossier des sources de la machine
const SOURCES: &str = "/sources";
/// Requêtes simultanées vers release-monitoring.org
const WORKERS: usize = 4;
const ANITYA: &str = "https://release-monitoring.org/api/v2";
const ADVISORIES: &str = "https://www.linuxfromscratch.org/advisories/index.php";

/// Nom d'archive → nom du paquet chez Arch Linux, quand ils diffèrent
const ALIASES: [(&str, &str); 17] = [
    ("llvm-project", "llvm"),
    ("linux-pam", "pam"),
    ("node", "nodejs"),
    ("vulkan-loader", "vulkan-icd-loader"),
    ("gtk", "gtk3"),
    ("inih", "libinih"),
    ("mako", "python-mako"),
    ("jetbrainsmono", "ttf-jetbrains-mono"),
    ("inter", "inter-font"),
    ("dejavu-fonts-ttf", "ttf-dejavu"),
    ("setuptools", "python-setuptools"),
    ("wheel", "python-wheel"),
    ("packaging", "python-packaging"),
    ("markupsafe", "python-markupsafe"),
    ("jinja2", "python-jinja"),
    ("pyyaml", "python-yaml"),
    ("flit_core", "python-flit-core"),
];

/// Un logiciel installé depuis ses sources
struct Installed {
    name: String,
    version: String,
    /// Parution de cette version (date de l'archive, gardée au téléchargement)
    released: String,
    /// Arrivée de l'archive sur la machine (changement d'état du fichier)
    installed: String,
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into()))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None | Some("verifier") => check(),
        Some("ignorer") if args.len() == 3 => {
            if let Err(e) = updates::ignore(&args[1], &args[2]) {
                eprintln!("odysseus-veille : {e}");
                std::process::exit(1);
            }
        }
        Some("etat") => print_state(),
        _ => {
            eprintln!("usage : odysseus-veille [verifier] | ignorer NOM VERSION | etat");
            std::process::exit(2);
        }
    }
}

fn print_state() {
    let Some(report) = updates::load() else {
        println!("Aucune vérification pour l'instant");
        return;
    };
    let pending = updates::pending(&report);
    println!(
        "Vérifié le {} : {} mise(s) à jour à décider",
        date_of(report.checked),
        pending.len()
    );
    for u in pending {
        let level = match u.level {
            Level::Security => "sécurité",
            Level::Fix => "correctif",
            Level::Release => "version",
        };
        println!("- {} {} → {} ({level})", u.name, u.installed, u.available);
    }
    for e in report.errors {
        println!("! {e}");
    }
}

fn date_of(secs: i64) -> String {
    chrono::DateTime::from_timestamp(secs, 0)
        .map(|d| {
            d.with_timezone(&chrono::Local)
                .format("%d/%m/%Y à %H:%M")
                .to_string()
        })
        .unwrap_or_default()
}

fn check() {
    let previous = updates::load();
    let mut errors = Vec::new();
    let mut list = Vec::new();

    if let Some(u) = check_odysseus(&mut errors) {
        list.push(u);
    }
    list.extend(check_rust());
    list.extend(check_system(&mut errors));

    let report = Report {
        checked: chrono::Utc::now().timestamp(),
        updates: list,
        errors,
    };
    if let Err(e) = updates::save(&report) {
        eprintln!("odysseus-veille : rapport non enregistré ({e})");
        std::process::exit(1);
    }
    announce(previous.as_ref(), &report);
}

/// Notification pour ce qui n'était pas dans le rapport précédent
fn announce(previous: Option<&Report>, report: &Report) {
    let known: HashSet<(String, String)> = previous
        .map(|r| {
            r.updates
                .iter()
                .map(|u| (u.name.clone(), u.available.clone()))
                .collect()
        })
        .unwrap_or_default();
    let fresh: Vec<Update> = updates::pending(report)
        .into_iter()
        .filter(|u| !known.contains(&(u.name.clone(), u.available.clone())))
        .collect();
    if fresh.is_empty() {
        return;
    }
    let security = fresh.iter().filter(|u| u.level == Level::Security).count();
    let title = match (fresh.len(), security) {
        (1, 1) => "Mise à jour de sécurité".to_string(),
        (1, _) => "Mise à jour disponible".to_string(),
        (n, 0) => format!("{n} mises à jour disponibles"),
        (n, s) => format!("{n} mises à jour, dont {s} de sécurité"),
    };
    let names: Vec<&str> = fresh.iter().take(5).map(|u| u.name.as_str()).collect();
    let more = if fresh.len() > 5 { ", …" } else { "" };
    let body = format!(
        "{}{more} — à voir dans Réglages > Mises à jour",
        names.join(", ")
    );
    let notify = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("odysseus-notify")))
        .filter(|p| p.exists());
    if let Some(notify) = notify {
        let mut cmd = Command::new(notify);
        // Lancé hors de la session (minuterie) : le bus de session de l'utilisateur
        if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none() {
            let runtime =
                std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| format!("/run/user/{}", uid()));
            cmd.env(
                "DBUS_SESSION_BUS_ADDRESS",
                format!("unix:path={runtime}/bus"),
            );
        }
        let _ = cmd.args(["--send", &title, &body]).status();
    }
}

/// Identifiant de l'utilisateur (sans dépendre de libc)
fn uid() -> u32 {
    std::fs::metadata("/proc/self")
        .map(|m| m.uid())
        .unwrap_or(1000)
}

// --- Réseau ---------------------------------------------------------------------

/// Page ou JSON par curl (installé partout ; rien à embarquer)
fn fetch(url: &str, form: Option<&str>) -> Result<String, String> {
    // Un serveur sollicité refuse parfois : trois essais, espacés
    let mut last = String::new();
    for attempt in 0..3 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_millis(1500 * attempt));
        }
        match fetch_once(url, form) {
            Ok(text) => return Ok(text),
            Err(e) => last = e,
        }
    }
    Err(last)
}

fn fetch_once(url: &str, form: Option<&str>) -> Result<String, String> {
    let mut cmd = Command::new("curl");
    cmd.args(["-sS", "-L", "--fail", "-m", "25", "-A", "odysseus-veille"]);
    if let Some(data) = form {
        cmd.args(["--data", data]);
    }
    let out = cmd.arg(url).output().map_err(|e| format!("curl : {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn fetch_json(url: &str) -> Result<Value, String> {
    serde_json::from_str(&fetch(url, None)?).map_err(|e| e.to_string())
}

// --- odysseus --------------------------------------------------------------------

/// Commits du dépôt distant pas encore récupérés
fn check_odysseus(errors: &mut Vec<String>) -> Option<Update> {
    let repo = home().join("odysseus-desktop");
    let git = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    };
    if git(&["fetch", "-q"]).is_none() {
        errors.push("odysseus : dépôt distant injoignable".into());
        return None;
    }
    let count: usize = git(&["rev-list", "--count", "HEAD..@{u}"])?.parse().ok()?;
    if count == 0 {
        return None;
    }
    let subjects = git(&["log", "--format=%s", "HEAD..@{u}"]).unwrap_or_default();
    let first = subjects.lines().next().unwrap_or_default();
    Some(Update {
        name: "odysseus".into(),
        kind: Kind::Odysseus,
        installed: git(&["rev-parse", "--short", "HEAD"]).unwrap_or_default(),
        available: git(&["rev-parse", "--short", "@{u}"]).unwrap_or_default(),
        level: Level::Release,
        advisories: Vec::new(),
        note: if count == 1 {
            first.to_string()
        } else {
            format!("{count} nouveautés, dont « {first} »")
        },
    })
}

// --- Outils ------------------------------------------------------------------------

/// Chaîne Rust : « stable-… - Update available : 1.90.0 (…) -> 1.91.0 (…) »
fn check_rust() -> Option<Update> {
    let rustup = home().join(".cargo/bin/rustup");
    let out = Command::new(&rustup).arg("check").output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text
        .lines()
        .find(|l| l.starts_with("stable") && l.contains("Update available"))?;
    let (_, versions) = line.split_once(':')?;
    let (from, to) = versions.split_once("->")?;
    let first = |s: &str| s.split_whitespace().next().unwrap_or_default().to_string();
    let (installed, available) = (first(from), first(to));
    Some(Update {
        name: "rust".into(),
        kind: Kind::Tool,
        level: if version::same_branch(&installed, &available) {
            Level::Fix
        } else {
            Level::Release
        },
        installed,
        available,
        advisories: Vec::new(),
        note: "rustup update".into(),
    })
}

// --- Système -----------------------------------------------------------------------

/// Archives de /sources (deux niveaux de dossiers) : la plus récente de chaque logiciel
fn inventory() -> Vec<Installed> {
    let mut found: BTreeMap<String, Installed> = BTreeMap::new();
    let mut dirs = vec![(PathBuf::from(SOURCES), 0)];
    while let Some((dir, depth)) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let Ok(meta) = e.metadata() else {
                continue;
            };
            if meta.is_dir() {
                if depth < 2 {
                    dirs.push((e.path(), depth + 1));
                }
                continue;
            }
            let file = e.file_name().to_string_lossy().into_owned();
            let Some((name, version)) = version::parse_archive(&file) else {
                continue;
            };
            let day = |secs: i64| {
                chrono::DateTime::from_timestamp(secs, 0)
                    .map(|d| d.format("%Y-%m-%d").to_string())
                    .unwrap_or_default()
            };
            let (released, installed) = (day(meta.mtime()), day(meta.ctime()));
            let key = name.to_lowercase();
            let newer = found
                .get(&key)
                .is_none_or(|old| version::compare(&version, &old.version).is_gt());
            if newer {
                found.insert(
                    key,
                    Installed {
                        name,
                        version,
                        released,
                        installed,
                    },
                );
            }
        }
    }
    found.into_values().collect()
}

/// Dernière version stable connue d'un logiciel (release-monitoring.org) :
/// d'abord par les paquets d'une distribution qui le suit, sinon par le
/// projet de ce nom (hors dépôts de bibliothèques de langages)
fn latest(name: &str) -> Option<String> {
    let key = name.to_lowercase();
    let stable = |v: &Value| {
        v["stable_version"]
            .as_str()
            .filter(|s| s.starts_with(|c: char| c.is_ascii_digit()))
            .map(String::from)
    };
    for (dist, query) in [
        ("Arch%20Linux", aliased(&key)),
        ("Fedora", aliased(&key)),
        ("Alpine", key.clone()),
    ] {
        let url = format!("{ANITYA}/packages/?name={query}&distribution={dist}");
        if let Some(v) = fetch_json(&url).ok().and_then(|v| stable(&v["items"][0])) {
            return Some(v);
        }
    }
    let v = fetch_json(&format!("{ANITYA}/projects/?name={key}")).ok()?;
    let projects: Vec<&Value> = v["items"]
        .as_array()?
        .iter()
        .filter(|p| {
            let eco = p["ecosystem"].as_str().unwrap_or_default();
            p["name"].as_str() == Some(key.as_str()) && eco.starts_with("http")
        })
        .collect();
    let [project] = projects.as_slice() else {
        return None;
    };
    project["stable_versions"][0]
        .as_str()
        .or_else(|| project["version"].as_str())
        .map(String::from)
}

/// Avis de sécurité LFS/BLFS des deux dernières éditions : (paquet en minuscules, avis)
fn advisories(errors: &mut Vec<String>) -> Vec<(String, Advisory)> {
    let index = match fetch(ADVISORIES, None) {
        Ok(t) => t,
        Err(e) => {
            errors.push(format!("avis de sécurité injoignables ({e})"));
            return Vec::new();
        }
    };
    // Éditions proposées par le formulaire, la plus récente d'abord
    let releases: Vec<String> = index
        .split("<option")
        .skip(1)
        .filter_map(|o| {
            let v = o.split('>').nth(1)?.split('<').next()?.trim().to_string();
            (v.contains('.') && v.chars().all(|c| c.is_ascii_digit() || c == '.')).then_some(v)
        })
        .take(2)
        .collect();
    let mut out = Vec::new();
    for release in releases {
        let form = format!("stype=Book&release={release}&submit=Search");
        let Ok(page) = fetch(ADVISORIES, Some(&form)) else {
            errors.push(format!("avis de sécurité {release} illisibles"));
            continue;
        };
        for row in page.split("<tr><td>").skip(1) {
            let cells: Vec<String> = row
                .split("<td>")
                .map(|c| c.split('<').next().unwrap_or_default().trim().to_string())
                .collect();
            if let [id, package, date, severity, ..] = cells.as_slice()
                && id.starts_with("sa-")
            {
                out.push((
                    package.to_lowercase(),
                    Advisory {
                        id: id.clone(),
                        date: date.clone(),
                        severity: severity.clone(),
                    },
                ));
            }
        }
    }
    out
}

/// Noyau : même branche sur kernel.org, ou branche terminée
fn check_kernel(installed: &Installed) -> Option<Update> {
    let v = fetch_json("https://www.kernel.org/releases.json").ok()?;
    let releases = v["releases"].as_array()?;
    let branch = |s: &str| s.split('.').take(2).collect::<Vec<_>>().join(".");
    let mine = branch(&installed.version);
    let same = releases
        .iter()
        .filter_map(|r| r["version"].as_str())
        .find(|r| branch(r) == mine && !r.contains("rc"));
    let stable = releases
        .iter()
        .find(|r| r["moniker"] == "stable")
        .and_then(|r| r["version"].as_str())?;
    let (available, note) = match same {
        Some(s) if version::compare(s, &installed.version).is_gt() => {
            (s.to_string(), format!("correctif de la branche {mine}"))
        }
        Some(_) => return None,
        None => (
            stable.to_string(),
            format!("la branche {mine} n'est plus suivie : passer à {stable}"),
        ),
    };
    Some(Update {
        name: installed.name.clone(),
        kind: Kind::System,
        installed: installed.version.clone(),
        available,
        level: Level::Fix,
        advisories: Vec::new(),
        note,
    })
}

/// Firefox ESR : dernière version de la branche ESR chez Mozilla
fn firefox_esr() -> Option<String> {
    fetch_json("https://product-details.mozilla.org/1.0/firefox_versions.json")
        .ok()?
        .get("FIREFOX_ESR")?
        .as_str()
        .map(String::from)
}

fn check_system(errors: &mut Vec<String>) -> Vec<Update> {
    let installed = inventory();
    if installed.is_empty() {
        return Vec::new();
    }
    let advisories = advisories(errors);
    let next = Mutex::new(0usize);
    let found = Mutex::new(Vec::new());
    let unknown = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..WORKERS {
            scope.spawn(|| {
                loop {
                    let i = {
                        let mut n = next.lock().unwrap();
                        *n += 1;
                        *n - 1
                    };
                    let Some(pkg) = installed.get(i) else {
                        break;
                    };
                    let key = pkg.name.to_lowercase();
                    if key == "linux" {
                        found.lock().unwrap().extend(check_kernel(pkg));
                        continue;
                    }
                    let latest = if key == "firefox" {
                        firefox_esr()
                    } else {
                        latest(&pkg.name)
                    };
                    if latest.is_none() {
                        unknown.lock().unwrap().push(pkg.name.clone());
                    }
                    let newer = latest
                        .as_ref()
                        .filter(|l| version::compare(l, &pkg.version).is_gt());
                    // Avis qui concernent la version installée : parus après
                    // elle quand une version plus récente existe (elle corrige
                    // sans doute) ; sinon seulement après l'installation (un
                    // avis plus ancien recommandait sans doute cette version)
                    let since = if newer.is_some() {
                        &pkg.released
                    } else {
                        &pkg.installed
                    };
                    let names = [key.clone(), aliased(&key)];
                    let recent: Vec<Advisory> = advisories
                        .iter()
                        .filter(|(p, a)| names.contains(p) && a.date > *since)
                        .map(|(_, a)| a.clone())
                        .collect();
                    if newer.is_none() && recent.is_empty() {
                        continue;
                    }
                    let available = newer.cloned().unwrap_or_else(|| pkg.version.clone());
                    let level = if !recent.is_empty() {
                        Level::Security
                    } else if version::same_branch(&pkg.version, &available) {
                        Level::Fix
                    } else {
                        Level::Release
                    };
                    let note = if newer.is_none() {
                        "avis de sécurité paru depuis l'installation ; pas encore de version plus récente connue".into()
                    } else {
                        String::new()
                    };
                    found.lock().unwrap().push(Update {
                        name: pkg.name.clone(),
                        kind: Kind::System,
                        installed: pkg.version.clone(),
                        available,
                        level,
                        advisories: recent,
                        note,
                    });
                }
            });
        }
    });
    let mut unknown = unknown.into_inner().unwrap();
    if !unknown.is_empty() {
        unknown.sort();
        errors.push(format!("sans source de versions : {}", unknown.join(", ")));
    }
    found.into_inner().unwrap()
}

/// Nom du paquet chez Arch (les avis LFS emploient parfois l'un, parfois l'autre)
fn aliased(key: &str) -> String {
    ALIASES
        .iter()
        .find(|(k, _)| *k == key)
        .map_or(key, |(_, a)| a)
        .to_string()
}
