//! Noms et versions : lecture d'un nom d'archive source, comparaison de versions.

use std::cmp::Ordering;

/// Archives de documentation, polices et outils sans version suivie
const SKIP: [&str; 7] = [
    "glslang",
    "systemd-man-pages",
    "sqlite-doc",
    "python-docs",
    "cargo-c-linux-musl",
    "bibata-modern-classic",
    "lucide-static",
];

/// Extensions d'archives reconnues
const EXTS: [&str; 7] = [
    ".tar.xz", ".tar.gz", ".tar.bz2", ".tar.zst", ".tgz", ".zip", ".tar",
];

/// « openssl-4.0.1.tar.gz » → (« openssl », « 4.0.1 »). Gère les formes
/// particulières des archives LFS : expect5.45.4, tcl8.6.18-src, unzip60,
/// tzdata2026c, sqlite-autoconf-3530400, node-v24.21.0, inih-r62,
/// llvm-project-22.1.8.src, Vulkan-Headers-vulkan-sdk-1.4.357.0…
pub fn parse_archive(file: &str) -> Option<(String, String)> {
    let stem = EXTS.iter().find_map(|e| file.strip_suffix(e))?;
    let bytes = stem.as_bytes();
    let digit = |i: usize| bytes.get(i).is_some_and(|b| b.is_ascii_digit());
    // Premier « - » suivi d'un chiffre (ou de v1, r62)
    let dash = (0..bytes.len()).find(|&i| {
        bytes[i] == b'-'
            && (digit(i + 1) || (matches!(bytes.get(i + 1), Some(b'v' | b'r')) && digit(i + 2)))
    });
    let (name, mut version) = match dash {
        Some(i) => {
            let v = &stem[i + 1..];
            let v = v
                .strip_prefix(['v', 'r'])
                .filter(|_| !digit(i + 1))
                .unwrap_or(v);
            (stem[..i].to_string(), v.to_string())
        }
        // Collé au nom : expect5.45.4, unzip60, tzdata2026c
        None => {
            let i = (1..bytes.len()).find(|&i| digit(i) && !digit(i - 1))?;
            (stem[..i].to_string(), stem[i..].to_string())
        }
    };
    let name = name
        .strip_suffix("-vulkan-sdk")
        .or_else(|| name.strip_suffix("-autoconf"))
        .unwrap_or(&name)
        .to_string();
    for suffix in [".source", ".src", "-src", "-stable"] {
        if let Some(v) = version.strip_suffix(suffix) {
            version = v.to_string();
        }
    }
    let key = name.to_lowercase();
    if version.contains("doc") || version.contains("html") || SKIP.contains(&key.as_str()) {
        return None;
    }
    // sqlite-autoconf-3530400 : 3.53.4
    if key == "sqlite" && version.len() == 7 && version.bytes().all(|b| b.is_ascii_digit()) {
        let n: u32 = version.parse().ok()?;
        version = format!(
            "{}.{}.{}",
            n / 1_000_000,
            (n / 10_000) % 100,
            (n / 100) % 100
        );
    }
    // unzip60, zip30 : 6.0, 3.0
    if (key == "unzip" || key == "zip") && version.len() == 2 {
        version = format!("{}.{}", &version[..1], &version[1..]);
    }
    Some((name, version))
}

#[derive(PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Token {
    /// Préversion (alpha, beta, rc) : avant la version finale
    Pre(String),
    Num(u64),
    Word(String),
}

fn tokens(v: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let flush = |cur: &mut String, out: &mut Vec<Token>| {
        if cur.is_empty() {
            return;
        }
        let t = if cur.bytes().all(|b| b.is_ascii_digit()) {
            Token::Num(cur.parse().unwrap_or(u64::MAX))
        } else if ["alpha", "beta", "rc", "pre"].contains(&cur.as_str()) {
            Token::Pre(cur.clone())
        } else {
            Token::Word(cur.clone())
        };
        out.push(t);
        cur.clear();
    };
    for c in v.to_lowercase().chars() {
        let same = cur
            .chars()
            .last()
            .is_none_or(|l| l.is_ascii_digit() == c.is_ascii_digit());
        if !c.is_ascii_alphanumeric() {
            flush(&mut cur, &mut out);
        } else {
            if !same {
                flush(&mut cur, &mut out);
            }
            cur.push(c);
        }
    }
    flush(&mut cur, &mut out);
    // « esr » (Firefox) n'entre pas dans la comparaison
    out.retain(|t| *t != Token::Word("esr".into()));
    out
}

/// Ordre de deux versions : 1.10 > 1.9, 3.0 > 3.0rc1, 1.9.17p2 = 1.9.17.p2
pub fn compare(a: &str, b: &str) -> Ordering {
    let (ta, tb) = (tokens(a), tokens(b));
    for i in 0..ta.len().max(tb.len()) {
        let o = match (ta.get(i), tb.get(i)) {
            (Some(x), Some(y)) => x.cmp(y),
            // Ce qui reste : une préversion est avant, le reste après
            (Some(Token::Pre(_)), None) => Ordering::Less,
            (Some(_), None) => Ordering::Greater,
            (None, Some(Token::Pre(_))) => Ordering::Greater,
            (None, Some(_)) => Ordering::Less,
            (None, None) => Ordering::Equal,
        };
        if o != Ordering::Equal {
            return o;
        }
    }
    Ordering::Equal
}

/// Même branche (deux premiers nombres égaux) : une version corrective
pub fn same_branch(a: &str, b: &str) -> bool {
    let nums = |v: &str| {
        tokens(v)
            .into_iter()
            .filter_map(|t| match t {
                Token::Num(n) => Some(n),
                _ => None,
            })
            .take(2)
            .collect::<Vec<_>>()
    };
    let (na, nb) = (nums(a), nums(b));
    // Versions datées (20260916, 2026c) : pas de branche
    na.len() == 2 && na == nb && na[0] < 1000
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(f: &str) -> Option<(String, String)> {
        parse_archive(f)
    }

    fn pair(n: &str, v: &str) -> Option<(String, String)> {
        Some((n.into(), v.into()))
    }

    #[test]
    fn archives() {
        assert_eq!(p("openssl-4.0.1.tar.gz"), pair("openssl", "4.0.1"));
        assert_eq!(p("expect5.45.4.tar.gz"), pair("expect", "5.45.4"));
        assert_eq!(p("tcl8.6.18-src.tar.gz"), pair("tcl", "8.6.18"));
        assert_eq!(p("tcl8.6.18-html.tar.gz"), None);
        assert_eq!(p("unzip60.tar.gz"), pair("unzip", "6.0"));
        assert_eq!(p("tzdata2026c.tar.gz"), pair("tzdata", "2026c"));
        assert_eq!(
            p("sqlite-autoconf-3530400.tar.gz"),
            pair("sqlite", "3.53.4")
        );
        assert_eq!(p("node-v24.21.0.tar.xz"), pair("node", "24.21.0"));
        assert_eq!(p("inih-r62.tar.gz"), pair("inih", "62"));
        assert_eq!(
            p("llvm-project-22.1.8.src.tar.xz"),
            pair("llvm-project", "22.1.8")
        );
        assert_eq!(
            p("firefox-153.5.0esr.source.tar.xz"),
            pair("firefox", "153.5.0esr")
        );
        assert_eq!(
            p("libevent-2.1.12-stable.tar.gz"),
            pair("libevent", "2.1.12")
        );
        assert_eq!(
            p("Vulkan-Headers-vulkan-sdk-1.4.357.0.tar.gz"),
            pair("Vulkan-Headers", "1.4.357.0")
        );
        assert_eq!(p("Linux-PAM-1.7.1.tar.xz"), pair("Linux-PAM", "1.7.1"));
        assert_eq!(p("python-3.14.7-docs-html.tar.bz2"), None);
        assert_eq!(p("poppler-26.10.0.tar.xz.sig"), None);
        assert_eq!(p("Bibata-Modern-Classic.tar.xz"), None);
    }

    #[test]
    fn ordering() {
        assert_eq!(compare("1.10", "1.9"), Ordering::Greater);
        assert_eq!(compare("3.0", "3.0rc1"), Ordering::Greater);
        assert_eq!(compare("1.9.17p2", "1.9.17.p2"), Ordering::Equal);
        assert_eq!(compare("153.5.0esr", "153.6.0esr"), Ordering::Less);
        assert_eq!(compare("3.6a", "3.6"), Ordering::Greater);
        assert_eq!(compare("2026c", "2026b"), Ordering::Greater);
        assert!(same_branch("4.0.1", "4.0.3"));
        assert!(!same_branch("4.0.1", "4.1.0"));
        assert!(!same_branch("20260916", "20261001"));
    }
}
