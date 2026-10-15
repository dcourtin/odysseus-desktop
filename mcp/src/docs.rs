//! Documentation pour l'agent : man pages, documentation du noyau, des
//! paquets, manuels info, notes de construction de la machine et
//! bibliothèque de l'utilisateur (~/Documents/Bibliothèque, PDF compris).
//!
//! Les textes longs ne sont jamais renvoyés d'un bloc : l'agent demande une
//! partie (titre de section), cherche un terme, ou lit page par page.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

/// Taille d'un extrait renvoyé (environ 5 000 jetons)
pub const LIMIT: usize = 20_000;

// --- Mise en forme commune ----------------------------------------------------

/// Titres d'un texte : (numéro de ligne, titre)
fn headings(lines: &[&str], kind: Kind) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim_end();
        let title = match kind {
            // Man page : titre en capitales, sans retrait
            Kind::Man => (!t.is_empty()
                && !t.starts_with(' ')
                && t.chars().count() < 60
                && !t.chars().any(|c| c.is_lowercase())
                && t.chars().any(|c| c.is_alphabetic())
                && i > 0
                && i + 1 < lines.len())
            .then(|| t.to_string()),
            Kind::Doc => {
                if let Some(h) = t.strip_prefix('#') {
                    Some(h.trim_start_matches('#').trim().to_string())
                } else {
                    // reStructuredText, info : ligne soulignée de = - ~ ^ *
                    let next = lines.get(i + 1).map(|l| l.trim_end()).unwrap_or("");
                    let underline = next.len() >= 3
                        && next.chars().all(|c| "=-~^*#".contains(c))
                        && next.chars().count() + 2 >= t.chars().count();
                    (underline && !t.is_empty() && !t.chars().all(|c| "=-~^*#".contains(c)))
                        .then(|| t.trim().to_string())
                }
            }
        }
        .filter(|h| !h.is_empty());
        if let Some(h) = title {
            out.push((i, h));
        }
    }
    out
}

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Man,
    Doc,
}

fn fold(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| match c {
            'à' | 'â' | 'ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'î' | 'ï' => 'i',
            'ô' | 'ö' => 'o',
            'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            c => c,
        })
        .collect()
}

/// Extrait d'un long texte : une partie, les passages qui contiennent un
/// terme, ou une page à partir d'un caractère donné.
pub fn excerpt(
    text: &str,
    kind: Kind,
    part: Option<&str>,
    search: Option<&str>,
    start: usize,
) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let heads = headings(&lines, kind);
    let list = || {
        heads
            .iter()
            .map(|(_, h)| h.as_str())
            .collect::<Vec<_>>()
            .join(" · ")
    };
    if let Some(part) = part.filter(|p| !p.trim().is_empty()) {
        let want = fold(part.trim());
        let found = heads
            .iter()
            .position(|(_, h)| fold(h) == want)
            .or_else(|| heads.iter().position(|(_, h)| fold(h).contains(&want)));
        return match found {
            Some(k) => {
                let from = heads[k].0;
                let to = heads.get(k + 1).map(|h| h.0).unwrap_or(lines.len());
                cut(&lines[from..to].join("\n"), 0)
            }
            None => format!("Partie « {part} » introuvable. Parties : {}", list()),
        };
    }
    if let Some(term) = search.filter(|s| !s.trim().is_empty()) {
        let want = fold(term.trim());
        // Paragraphes (séparés par une ligne vide) qui contiennent le terme
        let mut found = Vec::new();
        let mut para: Vec<&str> = Vec::new();
        let flush = |para: &mut Vec<&str>, found: &mut Vec<String>| {
            let p = para.join("\n");
            if fold(&p).contains(&want) {
                found.push(p);
            }
            para.clear();
        };
        for l in &lines {
            if l.trim().is_empty() {
                flush(&mut para, &mut found);
            } else {
                para.push(l);
            }
        }
        flush(&mut para, &mut found);
        if found.is_empty() {
            return format!("« {term} » n'apparaît pas. Parties : {}", list());
        }
        let n = found.len();
        let shown: Vec<String> = found.into_iter().take(15).collect();
        let mut out = format!("{n} passage(s) contenant « {term} » :\n\n");
        out.push_str(&shown.join("\n\n…\n\n"));
        return cut(&out, 0);
    }
    let total = text.chars().count();
    let mut out = cut(text, start);
    if total > LIMIT && start == 0 && !heads.is_empty() {
        out = format!("Parties : {}\n\n{out}", list());
    }
    out
}

/// Au plus LIMIT caractères à partir de `start`, avec de quoi lire la suite
fn cut(text: &str, start: usize) -> String {
    let total = text.chars().count();
    let body: String = text.chars().skip(start).take(LIMIT).collect();
    let end = start + body.chars().count();
    if end < total {
        format!(
            "{body}\n\n[… suite : caractères {end} à {total} ; relire avec debut={end}, ou demander une partie]"
        )
    } else {
        body
    }
}

// --- Man pages --------------------------------------------------------------------

fn valid_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() < 80
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-+:@".contains(c))
        && !s.starts_with('-')
}

/// Man page mise en texte, sans césure ni justification
pub fn manual(
    page: &str,
    section: Option<&str>,
    english: bool,
    part: Option<&str>,
    search: Option<&str>,
    start: usize,
) -> Result<String, String> {
    if !valid_name(page) {
        return Err(format!("Nom de page invalide : {page}"));
    }
    let mut cmd = Command::new("man");
    cmd.env("MANWIDTH", "100")
        .env("MAN_KEEP_FORMATTING", "")
        .env_remove("MANPAGER")
        .env_remove("PAGER")
        .args(["--nh", "--nj", "-P", "cat"]);
    if english {
        cmd.env("LC_ALL", "C.UTF-8").env("LANGUAGE", "en");
    }
    if let Some(s) = section.filter(|s| !s.is_empty()) {
        if !valid_name(s) {
            return Err(format!("Section invalide : {s}"));
        }
        cmd.arg(s);
    }
    let out = cmd.arg(page).output().map_err(|e| format!("man : {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() || text.trim().is_empty() {
        return Err(format!(
            "Pas de man page « {page} »{}. Essayez « apropos » pour la trouver.",
            section
                .map(|s| format!(" en section {s}"))
                .unwrap_or_default()
        ));
    }
    Ok(excerpt(&clean(&text), Kind::Man, part, search, start))
}

/// Retire les caractères gras et soulignés (x\bx) et les codes de couleur
fn clean(s: &str) -> String {
    let mut out: Vec<char> = Vec::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{8}' => {
                out.pop();
            }
            '\u{1b}' => {
                for d in chars.by_ref() {
                    if d.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            c => out.push(c),
        }
    }
    out.into_iter().collect()
}

/// Pages dont le nom ou la description contient tous les mots donnés
pub fn apropos(words: &str) -> Result<String, String> {
    let words: Vec<&str> = words.split_whitespace().take(6).collect();
    if words.is_empty() {
        return Err("Mots-clés manquants".into());
    }
    let out = Command::new("apropos")
        .arg("-a")
        .args(&words)
        .output()
        .map_err(|e| format!("apropos : {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return Ok(format!("Aucune page pour « {} »", words.join(" ")));
    }
    let mut res = lines
        .iter()
        .take(60)
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    if lines.len() > 60 {
        res.push_str(&format!(
            "\n… et {} autres : précisez la recherche",
            lines.len() - 60
        ));
    }
    Ok(res)
}

// --- Documentation -----------------------------------------------------------------

/// Sources de documentation : (nom, dossier ou fichier, description)
pub fn sources() -> Vec<(&'static str, PathBuf, &'static str)> {
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_default());
    let mut v = vec![
        (
            "machine",
            PathBuf::from("/sources/blfs-desktop"),
            "comment cette machine a été construite : STATUS.md (journal), PLAN.md, scripts build-*.sh",
        ),
        (
            "odysseus",
            home.join("odysseus-desktop/README.md"),
            "le bureau odysseus-desktop : composants, raccourcis, commandes",
        ),
        (
            "noyau",
            PathBuf::from("/sources/linux-7.1.8/Documentation"),
            "documentation du noyau Linux 7.1.8",
        ),
        (
            "paquets",
            PathBuf::from("/usr/share/doc"),
            "documentation installée avec les logiciels",
        ),
        ("info", PathBuf::from("/usr/share/info"), "manuels GNU info"),
    ];
    v.push((
        "bibliotheque",
        library(),
        "livres et documents déposés par l'utilisateur (PDF, texte, Markdown, HTML)",
    ));
    v
}

pub fn library() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default()).join("Documents/Bibliothèque")
}

const TEXT_EXT: [&str; 10] = [
    "md", "rst", "txt", "html", "htm", "info", "pdf", "sh", "conf", "",
];

fn readable(p: &Path) -> bool {
    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    // Fichiers info découpés : automake.info-1
    let info_part = p
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.contains(".info-"));
    (TEXT_EXT.contains(&ext.as_str()) || info_part)
        && p.metadata().is_ok_and(|m| m.len() < 30_000_000)
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    if depth > 8 || out.len() > 40_000 {
        return;
    }
    if dir.is_file() {
        out.push(dir.to_path_buf());
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        let name = e.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || name == "build" || name == "logs" {
            continue;
        }
        match e.file_type() {
            Ok(t) if t.is_dir() => walk(&p, out, depth + 1),
            Ok(t) if t.is_file() && readable(&p) => out.push(p),
            _ => {}
        }
    }
}

/// Texte d'un document : PDF par pdftotext (gardé en cache), HTML sans balises
pub fn read_doc(p: &Path) -> Result<String, String> {
    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "pdf" => pdf_text(p),
        "html" | "htm" => {
            let raw = std::fs::read(p).map_err(|e| e.to_string())?;
            Ok(html_text(&String::from_utf8_lossy(&raw)))
        }
        _ => {
            let raw = std::fs::read(p).map_err(|e| e.to_string())?;
            if raw.iter().take(4096).any(|b| *b == 0) {
                return Err("Fichier binaire".into());
            }
            Ok(String::from_utf8_lossy(&raw).into_owned())
        }
    }
}

fn pdf_text(p: &Path) -> Result<String, String> {
    let home = std::env::var("HOME").unwrap_or_default();
    let cache = PathBuf::from(home).join(".cache/odysseus/bibliotheque");
    let key = p.to_string_lossy().replace('/', "_");
    let out = cache.join(format!("{key}.txt"));
    let fresh = match (out.metadata(), p.metadata()) {
        (Ok(o), Ok(s)) => o.modified().ok() >= s.modified().ok(),
        _ => false,
    };
    if !fresh {
        std::fs::create_dir_all(&cache).map_err(|e| e.to_string())?;
        let status = Command::new("pdftotext")
            .args(["-enc", "UTF-8", "-nopgbrk"])
            .arg(p)
            .arg(&out)
            .status()
            .map_err(|e| format!("pdftotext : {e}"))?;
        if !status.success() {
            return Err("PDF illisible (pdftotext a échoué)".into());
        }
    }
    std::fs::read_to_string(&out).map_err(|e| e.to_string())
}

/// HTML → texte : titres gardés en « # », balises retirées, entités courantes
fn html_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let mut rest = html;
    let mut skip_until: Option<&str> = None;
    while let Some(lt) = rest.find('<') {
        if skip_until.is_none() {
            out.push_str(&rest[..lt]);
        }
        let Some(gt) = rest[lt..].find('>') else {
            break;
        };
        let tag = rest[lt + 1..lt + gt].to_ascii_lowercase();
        rest = &rest[lt + gt + 1..];
        let name = tag
            .trim_start_matches('/')
            .split(|c: char| c.is_whitespace() || c == '/')
            .next()
            .unwrap_or("");
        if let Some(end) = skip_until {
            if tag.starts_with('/') && name == end {
                skip_until = None;
            }
            continue;
        }
        match name {
            "script" | "style" if !tag.starts_with('/') => {
                skip_until = Some(if name == "script" { "script" } else { "style" })
            }
            "h1" | "h2" | "h3" | "h4" if !tag.starts_with('/') => out.push_str("\n\n# "),
            "p" | "div" | "br" | "li" | "tr" | "pre" | "h1" | "h2" | "h3" | "h4" | "dt" | "dd" => {
                out.push('\n')
            }
            _ => {}
        }
    }
    out.push_str(rest);
    let out = out
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&");
    // Lignes vides en série réduites
    let mut clean = String::with_capacity(out.len());
    let mut blank = 0;
    for l in out.lines() {
        if l.trim().is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        clean.push_str(l.trim_end());
        clean.push('\n');
    }
    clean
}

/// Le chemin est-il dans une des sources ?
pub fn in_sources(p: &Path) -> Option<PathBuf> {
    let real = p.canonicalize().ok()?;
    sources()
        .iter()
        .filter_map(|(_, root, _)| root.canonicalize().ok())
        .any(|root| real.starts_with(&root))
        .then_some(real)
}

/// Recherche dans les sources : chaque mot doit apparaître (nom ou contenu)
pub fn search(query: &str, source: Option<&str>) -> Result<String, String> {
    let words: Vec<String> = query
        .split_whitespace()
        .map(fold)
        .filter(|w| w.chars().count() >= 2)
        .take(8)
        .collect();
    if words.is_empty() {
        return Err("Recherche vide".into());
    }
    let all = sources();
    let chosen: Vec<_> = all
        .iter()
        .filter(|(name, ..)| source.is_none_or(|s| s.is_empty() || s == "tout" || *name == s))
        .collect();
    if chosen.is_empty() {
        return Err(format!(
            "Source inconnue : {} ({})",
            source.unwrap_or(""),
            all.iter().map(|s| s.0).collect::<Vec<_>>().join(", ")
        ));
    }
    // Une source par fil : la documentation du noyau ne retarde pas les autres
    let mut hits: Vec<(i64, PathBuf, String, &str)> = std::thread::scope(|scope| {
        let jobs: Vec<_> = chosen
            .iter()
            .map(|(name, root, _)| {
                let words = &words;
                scope.spawn(move || search_source(name, root, words))
            })
            .collect();
        jobs.into_iter()
            .flat_map(|j| j.join().unwrap_or_default())
            .collect()
    });
    if hits.is_empty() {
        return Ok(format!(
            "Rien trouvé pour « {query} ». Essayez d'autres mots (en anglais pour le noyau), ou « apropos »."
        ));
    }
    hits.sort_by_key(|h| std::cmp::Reverse(h.0));
    let n = hits.len();
    let mut out = format!(
        "{n} document(s) pour « {query} », les plus pertinents d'abord (lire avec documentation et « chemin ») :\n"
    );
    for (_, path, snippet, src) in hits.into_iter().take(12) {
        out.push_str(&format!(
            "- [{src}] {}\n    « {snippet} »\n",
            path.display()
        ));
    }
    Ok(out)
}

/// Documents d'une source qui contiennent tous les mots : (score, chemin, passage, source)
fn search_source<'a>(
    name: &'a str,
    root: &Path,
    words: &[String],
) -> Vec<(i64, PathBuf, String, &'a str)> {
    let mut hits = Vec::new();
    let mut files = Vec::new();
    walk(root, &mut files, 0);
    for f in files {
        let path_text = fold(&f.to_string_lossy());
        let name_hits = words
            .iter()
            .filter(|w| path_text.contains(w.as_str()))
            .count();
        // Illisible (binaire, PDF en échec) : gardé seulement si son nom correspond
        let text = match read_doc(&f) {
            Ok(t) => t,
            Err(_) if name_hits == words.len() => String::new(),
            Err(_) => continue,
        };
        let lower = fold(&text);
        if !words
            .iter()
            .all(|w| lower.contains(w.as_str()) || path_text.contains(w.as_str()))
        {
            continue;
        }
        let count: usize = words
            .iter()
            .map(|w| lower.matches(w.as_str()).count().min(50))
            .sum();
        let score = name_hits as i64 * 100 + count as i64;
        // Passage le plus riche : la ligne qui contient le plus de mots
        let snippet = text
            .lines()
            .max_by_key(|l| {
                let fl = fold(l);
                words.iter().filter(|w| fl.contains(w.as_str())).count()
            })
            .unwrap_or("")
            .trim()
            .chars()
            .take(200)
            .collect::<String>();
        hits.push((score, f, snippet, name));
    }
    hits
}

/// Les sources, avec leur description (et l'état de la bibliothèque)
pub fn list_sources() -> String {
    let mut out = String::from("Sources de documentation :\n");
    for (name, path, desc) in sources() {
        let state = if path.exists() { "" } else { " (absente)" };
        out.push_str(&format!("- {name} : {desc} — {}{state}\n", path.display()));
    }
    let lib = library();
    let mut books = Vec::new();
    walk(&lib, &mut books, 0);
    if !books.is_empty() {
        out.push_str("Bibliothèque :\n");
        for b in books.iter().take(50) {
            out.push_str(&format!("- {}\n", b.display()));
        }
    }
    out.push_str("Man pages : outils « manuel » et « apropos ».");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parts_and_search() {
        let man = "EPOLL(7)  Manual  EPOLL(7)\n\nNAME\n     epoll - notif\n\nDESCRIPTION\n     The epoll API.\n\n     Edge triggered.\n\nSEE ALSO\n     poll(2)\n\nLinux  2026  EPOLL(7)";
        let d = excerpt(man, Kind::Man, Some("description"), None, 0);
        assert!(d.starts_with("DESCRIPTION") && d.contains("Edge") && !d.contains("poll(2)"));
        let s = excerpt(man, Kind::Man, None, Some("edge"), 0);
        assert!(s.contains("Edge triggered") && !s.contains("NAME"));
        let doc = "Titre\n=====\n\nIntro\n\nSuite\n-----\n\nfin";
        let p = excerpt(doc, Kind::Doc, Some("suite"), None, 0);
        assert!(p.starts_with("Suite") && p.contains("fin"));
    }

    #[test]
    fn html() {
        let t = html_text("<html><style>x{}</style><h2>Titre</h2><p>Un &amp; deux</p></html>");
        assert!(t.contains("# Titre") && t.contains("Un & deux") && !t.contains("x{}"));
    }

    #[test]
    fn long_text_is_paged() {
        let long = "a".repeat(LIMIT + 10);
        let first = excerpt(&long, Kind::Doc, None, None, 0);
        assert!(first.contains(&format!("debut={LIMIT}")));
        assert_eq!(excerpt(&long, Kind::Doc, None, None, LIMIT), "a".repeat(10));
    }
}
