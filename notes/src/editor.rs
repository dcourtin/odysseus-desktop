//! Le texte d'une note et son édition : curseur, sélection, annuler, et une
//! mise en forme Markdown légère, lue ligne à ligne.
//!
//! Le texte reste du Markdown brut ; l'affichage le met en forme sans rien
//! cacher, sauf les débuts de liste (« - », « - [ ] », « > ») remplacés par
//! une puce, une case ou un filet. Chaque caractère garde ainsi sa place à
//! l'écran, ce qui garde simples le curseur et la sélection.

use std::time::{Duration, Instant};

/// Style d'un caractère à l'écran
#[derive(Clone, Copy, PartialEq, Default, Debug)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub strike: bool,
    /// Caractère de syntaxe (« ** », « # »…) : montré en retrait
    pub marker: bool,
    /// Remplacé par un dessin (puce, case, filet)
    pub hidden: bool,
}

/// Nature d'un paragraphe (une ligne du texte)
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Block {
    Normal,
    /// Première ligne de la note, sans autre forme : son titre
    Title,
    Heading(u8),
    Bullet,
    Check(bool),
    Number,
    Quote,
    /// Ligne « ``` » qui ouvre ou ferme un bloc de code
    Fence,
    Code,
}

/// Début de ligne remplacé par un dessin, ou numéro : où commence le texte
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Prefix {
    /// Espaces d'indentation (listes)
    pub indent: usize,
    /// Octets du marqueur après l'indentation (« - [ ] » : 6)
    pub len: usize,
}

/// Lit la nature d'une ligne ; `first` : première ligne de la note,
/// `in_code` : à l'intérieur d'un bloc de code.
pub fn block(line: &str, first: bool, in_code: bool) -> (Block, Option<Prefix>) {
    if line.trim_start().starts_with("```") {
        return (Block::Fence, None);
    }
    if in_code {
        return (Block::Code, None);
    }
    let trimmed = line.trim_start_matches(' ');
    let indent = line.len() - trimmed.len();
    let prefix = |len| Some(Prefix { indent, len });
    for (marker, done) in [("- [ ] ", false), ("- [x] ", true), ("- [X] ", true)] {
        if trimmed.starts_with(marker) || trimmed == marker.trim_end() {
            return (Block::Check(done), prefix(marker.len().min(trimmed.len())));
        }
    }
    if trimmed.starts_with("- ") || trimmed.starts_with("* ") {
        return (Block::Bullet, prefix(2));
    }
    let digits = trimmed.bytes().take_while(u8::is_ascii_digit).count();
    if digits > 0 && trimmed[digits..].starts_with(". ") {
        return (Block::Number, prefix(digits + 2));
    }
    if indent == 0 {
        if trimmed.starts_with("> ") {
            return (Block::Quote, prefix(2));
        }
        let hashes = line.bytes().take_while(|b| *b == b'#').count();
        if (1..=3).contains(&hashes) && line[hashes..].starts_with(' ') {
            return (Block::Heading(hashes as u8), None);
        }
    }
    if first {
        return (Block::Title, None);
    }
    (Block::Normal, None)
}

/// Style de chaque caractère d'une ligne (gras, italique, code, barré, marqueurs)
pub fn inline(line: &str, block: Block, prefix: Option<Prefix>) -> Vec<Style> {
    let chars: Vec<char> = line.chars().collect();
    let mut st = vec![Style::default(); chars.len()];
    match block {
        Block::Code => {
            st.iter_mut().for_each(|s| s.code = true);
            return st;
        }
        Block::Fence => {
            st.iter_mut().for_each(|s| {
                s.code = true;
                s.marker = true;
            });
            return st;
        }
        Block::Heading(n) => {
            st.iter_mut().for_each(|s| s.bold = true);
            for s in st.iter_mut().take(n as usize + 1) {
                s.marker = true;
            }
        }
        Block::Title => st.iter_mut().for_each(|s| s.bold = true),
        _ => {}
    }
    // Le début de liste n'a pas de mise en forme en ligne
    let start = prefix.map_or(0, |p| p.indent + p.len);
    if let Some(p) = prefix {
        let hide = !matches!(block, Block::Number);
        for s in st.iter_mut().take(p.indent + p.len) {
            s.hidden = hide;
            s.marker = !hide;
        }
    }
    let mut taken = vec![false; chars.len()];
    // Code d'abord : rien n'est mis en forme à l'intérieur
    pair(&chars, &mut st, &mut taken, start, "`", |s| s.code = true);
    pair(&chars, &mut st, &mut taken, start, "**", |s| s.bold = true);
    pair(&chars, &mut st, &mut taken, start, "~~", |s| {
        s.strike = true
    });
    pair(&chars, &mut st, &mut taken, start, "*", |s| s.italic = true);
    pair(&chars, &mut st, &mut taken, start, "_", |s| s.italic = true);
    if let Block::Check(true) = block {
        for s in st.iter_mut().skip(start) {
            s.strike = true;
        }
    }
    st
}

/// Paires de délimiteurs (`d` … `d`) sur une ligne : marqueurs en retrait,
/// contenu mis en forme par `apply`.
fn pair(
    chars: &[char],
    st: &mut [Style],
    taken: &mut [bool],
    start: usize,
    d: &str,
    apply: impl Fn(&mut Style),
) {
    let d: Vec<char> = d.chars().collect();
    let n = d.len();
    let at = |i: usize, taken: &[bool]| {
        i + n <= chars.len() && chars[i..i + n] == d[..] && !taken[i..i + n].iter().any(|t| *t)
    };
    // « _ » au milieu d'un mot (nom_de_fichier) n'est pas de l'italique
    let word = |i: usize| chars.get(i).is_some_and(|c| c.is_alphanumeric());
    let mut i = start;
    while i < chars.len() {
        let open = at(i, taken)
            && !(d[0] == '_' && i > 0 && word(i - 1))
            && chars.get(i + n).is_some_and(|c| !c.is_whitespace());
        if !open {
            i += 1;
            continue;
        }
        let mut j = i + n + 1;
        let close = loop {
            if j + n > chars.len() {
                break None;
            }
            if at(j, taken)
                && !chars[j - 1].is_whitespace()
                && !(d[0] == '_' && word(j + n))
                // « ** » n'est pas la fin d'un « * »
                && !(n == 1 && d[0] == '*' && chars.get(j + 1) == Some(&'*'))
            {
                break Some(j);
            }
            j += 1;
        };
        let Some(j) = close else {
            i += 1;
            continue;
        };
        for k in i..j + n {
            taken[k] = true;
            if k < i + n || k >= j {
                st[k].marker = true;
            } else {
                apply(&mut st[k]);
            }
        }
        i = j + n;
    }
}

// --- Mise en page --------------------------------------------------------------

/// Mesure d'un caractère dans son style ; `px` : taille du texte
pub trait Metrics {
    fn advance(&self, style: &Style, block: Block, ch: char, px: f32) -> f32;
}

/// Tailles de texte, en pixels du tampon
#[derive(Clone, Copy)]
pub struct Sizes {
    pub body: f32,
    pub code: f32,
    pub title: f32,
    pub h1: f32,
    pub h2: f32,
    pub h3: f32,
    /// Retrait par niveau d'indentation (deux espaces)
    pub indent: f32,
    /// Largeur d'une puce ou d'une case, espace compris
    pub widget: f32,
}

impl Sizes {
    pub fn px(&self, block: Block) -> f32 {
        match block {
            Block::Title => self.title,
            Block::Heading(1) => self.h1,
            Block::Heading(2) => self.h2,
            Block::Heading(_) => self.h3,
            Block::Code | Block::Fence => self.code,
            _ => self.body,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Glyph {
    pub byte: usize,
    pub ch: char,
    pub x: f32,
    pub w: f32,
    pub style: Style,
}

/// Une ligne à l'écran (un paragraphe trop long en fait plusieurs)
#[derive(Clone, Debug)]
pub struct VLine {
    /// Octets [start, end) du texte ; la dernière ligne d'un paragraphe
    /// contient aussi la position `end` (avant le retour à la ligne)
    pub start: usize,
    pub end: usize,
    pub last: bool,
    /// Première ligne du paragraphe (là où se dessine la puce ou la case)
    pub first: bool,
    pub y: f32,
    pub h: f32,
    pub px: f32,
    pub block: Block,
    /// Début du texte (après l'indentation et la puce), pour les suites de ligne
    pub text_x: f32,
    /// Position de la puce ou de la case
    pub widget_x: f32,
    pub glyphs: Vec<Glyph>,
}

impl VLine {
    /// Abscisse de la position `byte`, prise dans cette ligne
    pub fn x_of(&self, byte: usize) -> f32 {
        for g in &self.glyphs {
            if g.byte >= byte {
                return g.x;
            }
        }
        self.glyphs.last().map_or(self.text_x, |g| g.x + g.w)
    }

    /// Position la plus proche de l'abscisse `x`
    pub fn byte_at(&self, x: f32) -> usize {
        for g in &self.glyphs {
            if g.style.hidden && g.w == 0.0 {
                continue;
            }
            if x < g.x + g.w / 2.0 {
                return g.byte;
            }
        }
        if self.last {
            self.end
        } else {
            // Fin d'une ligne coupée : avant l'espace de la coupure
            self.glyphs
                .last()
                .filter(|g| g.ch == ' ')
                .map_or(self.end, |g| g.byte)
        }
    }
}

pub struct Layout {
    pub lines: Vec<VLine>,
    pub height: f32,
}

impl Layout {
    /// Ligne qui contient la position `byte`
    pub fn line_of(&self, byte: usize) -> usize {
        self.lines
            .iter()
            .position(|l| byte >= l.start && (byte < l.end || (byte == l.end && l.last)))
            .unwrap_or(self.lines.len().saturating_sub(1))
    }

    /// Ligne sous l'ordonnée `y`
    pub fn line_at(&self, y: f32) -> usize {
        self.lines
            .iter()
            .position(|l| y < l.y + l.h)
            .unwrap_or(self.lines.len().saturating_sub(1))
    }

    pub fn point(&self, byte: usize) -> (f32, f32, f32) {
        match self.lines.get(self.line_of(byte)) {
            Some(l) => (l.x_of(byte), l.y, l.h),
            None => (0.0, 0.0, 0.0),
        }
    }

    pub fn byte_at(&self, x: f32, y: f32) -> usize {
        self.lines.get(self.line_at(y)).map_or(0, |l| l.byte_at(x))
    }
}

/// Coupe le texte en lignes d'au plus `width` pixels
pub fn layout(text: &str, m: &impl Metrics, sz: &Sizes, width: f32) -> Layout {
    let mut lines = Vec::new();
    let mut y = 0.0;
    let mut in_code = false;
    let mut start = 0;
    for (para, line) in text.split('\n').enumerate() {
        let (block, prefix) = block(line, para == 0, in_code);
        if block == Block::Fence {
            in_code = !in_code;
        }
        let styles = inline(line, block, prefix);
        let px = sz.px(block);
        let h = (px * 1.5).round();
        // Un peu d'air avant un intertitre
        if matches!(block, Block::Heading(_)) && para > 0 {
            y += (sz.body * 0.6).round();
        }
        // Largeur de chaque caractère ; le début de liste devient retrait + puce
        let mut glyphs = Vec::new();
        let (mut x, mut text_x, mut widget_x) = (0.0, 0.0, 0.0);
        let pre = prefix.map_or(0, |p| p.indent + p.len);
        for (i, ((byte, ch), style)) in line.char_indices().zip(styles).enumerate() {
            let w = match prefix {
                Some(p) if i < p.indent => sz.indent / 2.0,
                Some(_) if i == pre - 1 && style.hidden => sz.widget,
                _ if style.hidden => 0.0,
                _ => m.advance(&style, block, ch, px),
            };
            if i == pre.saturating_sub(1) && style.hidden {
                widget_x = x;
            }
            glyphs.push(Glyph {
                byte: start + byte,
                ch,
                x,
                w,
                style,
            });
            x += w;
            if i + 1 == pre {
                text_x = x;
            }
        }
        if prefix.is_some_and(|p| p.indent > 0) && pre > 0 && glyphs.len() < pre {
            text_x = x;
        }
        // Coupe aux espaces ; un mot plus long que la ligne est coupé net
        let mut from = 0;
        let mut first = true;
        loop {
            let mut cut = glyphs.len();
            let offset = if first { 0.0 } else { text_x };
            let base = glyphs.get(from).map_or(0.0, |g| g.x);
            for (k, g) in glyphs.iter().enumerate().skip(from) {
                if g.x - base + g.w + offset > width && k > from && g.ch != ' ' {
                    cut = glyphs[from..k]
                        .iter()
                        .rposition(|g| g.ch == ' ')
                        .map(|r| from + r + 1)
                        .filter(|c| *c > from.max(pre))
                        .unwrap_or(k);
                    break;
                }
            }
            let mut part: Vec<Glyph> = glyphs[from..cut].to_vec();
            for g in &mut part {
                g.x = g.x - base + offset;
            }
            let last = cut >= glyphs.len();
            lines.push(VLine {
                start: glyphs.get(from).map_or(start, |g| g.byte),
                end: if last {
                    start + line.len()
                } else {
                    glyphs[cut].byte
                },
                last,
                first,
                y,
                h,
                px,
                block,
                text_x: if first { text_x } else { offset },
                widget_x,
                glyphs: part,
            });
            y += h;
            if last {
                break;
            }
            from = cut;
            first = false;
        }
        start += line.len() + 1;
    }
    Layout { lines, height: y }
}

// --- Édition ---------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Edit {
    Type,
    Erase,
    Other,
}

struct Snapshot {
    text: String,
    cursor: usize,
    anchor: usize,
}

/// Forme appliquée par la barre d'outils
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Format {
    Bold,
    Italic,
    Strike,
    Code,
    Heading,
    Bullet,
    Check,
}

pub struct Editor {
    pub text: String,
    pub cursor: usize,
    /// Autre bout de la sélection (égal au curseur : pas de sélection)
    pub anchor: usize,
    /// Abscisse visée par ↑ et ↓, gardée d'une ligne à l'autre
    pub goal_x: Option<f32>,
    /// Modifié depuis le dernier enregistrement
    pub changed: bool,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    last: Option<(Edit, Instant)>,
}

const UNDO_MAX: usize = 300;

impl Editor {
    pub fn new(text: String) -> Self {
        let end = text.len();
        Self {
            text,
            cursor: end,
            anchor: end,
            goal_x: None,
            changed: false,
            undo: Vec::new(),
            redo: Vec::new(),
            last: None,
        }
    }

    pub fn selection(&self) -> Option<(usize, usize)> {
        (self.cursor != self.anchor)
            .then(|| (self.cursor.min(self.anchor), self.cursor.max(self.anchor)))
    }

    pub fn selected_text(&self) -> Option<&str> {
        self.selection().map(|(a, b)| &self.text[a..b])
    }

    /// Mémorise l'état avant une modification ; la frappe continue (moins
    /// d'une seconde d'écart) s'annule d'un bloc.
    fn checkpoint(&mut self, kind: Edit) {
        let merge = matches!(self.last, Some((k, at)) if k == kind && kind != Edit::Other
            && at.elapsed() < Duration::from_secs(1));
        if !merge {
            self.undo.push(Snapshot {
                text: self.text.clone(),
                cursor: self.cursor,
                anchor: self.anchor,
            });
            if self.undo.len() > UNDO_MAX {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
        self.last = Some((kind, Instant::now()));
        self.changed = true;
        self.goal_x = None;
    }

    pub fn undo(&mut self) {
        if let Some(s) = self.undo.pop() {
            let now = std::mem::replace(&mut self.text, s.text);
            self.redo.push(Snapshot {
                text: now,
                cursor: self.cursor,
                anchor: self.anchor,
            });
            (self.cursor, self.anchor) = (s.cursor, s.anchor);
            self.after_history();
        }
    }

    pub fn redo(&mut self) {
        if let Some(s) = self.redo.pop() {
            let now = std::mem::replace(&mut self.text, s.text);
            self.undo.push(Snapshot {
                text: now,
                cursor: self.cursor,
                anchor: self.anchor,
            });
            (self.cursor, self.anchor) = (s.cursor, s.anchor);
            self.after_history();
        }
    }

    fn after_history(&mut self) {
        self.cursor = self.cursor.min(self.text.len());
        self.anchor = self.anchor.min(self.text.len());
        self.last = None;
        self.changed = true;
        self.goal_x = None;
    }

    /// Remplace [a, b) par `s` ; le curseur se place après
    fn splice(&mut self, a: usize, b: usize, s: &str) {
        self.text.replace_range(a..b, s);
        self.cursor = a + s.len();
        self.anchor = self.cursor;
    }

    pub fn insert(&mut self, s: &str) {
        let kind = if s.chars().count() == 1 && s != "\n" {
            Edit::Type
        } else {
            Edit::Other
        };
        self.checkpoint(kind);
        let (a, b) = self.selection().unwrap_or((self.cursor, self.cursor));
        self.splice(a, b, s);
    }

    /// Début et fin (sans le retour à la ligne) du paragraphe qui contient `byte`
    pub fn paragraph(&self, byte: usize) -> (usize, usize) {
        let a = self.text[..byte].rfind('\n').map_or(0, |i| i + 1);
        let b = self.text[byte..]
            .find('\n')
            .map_or(self.text.len(), |i| byte + i);
        (a, b)
    }

    fn line_block(&self, a: usize) -> (Block, Option<Prefix>) {
        let (_, b) = self.paragraph(a);
        let in_code = self.text[..a]
            .lines()
            .filter(|l| l.trim_start().starts_with("```"))
            .count()
            % 2
            == 1;
        block(&self.text[a..b], a == 0, in_code)
    }

    /// Entrée : une liste continue avec un nouvel élément ; un élément vide la termine
    pub fn newline(&mut self) {
        if self.selection().is_some() {
            return self.insert("\n");
        }
        let (a, b) = self.paragraph(self.cursor);
        let (blk, prefix) = self.line_block(a);
        let Some(p) = prefix else {
            return self.insert("\n");
        };
        let line = &self.text[a..b];
        let marker_end = a + p.indent + p.len;
        if self.cursor < marker_end {
            return self.insert("\n");
        }
        if line[p.indent + p.len..].trim().is_empty() {
            // Élément vide : fin de la liste
            self.checkpoint(Edit::Other);
            self.text.replace_range(a..b, "");
            self.cursor = a;
            self.anchor = a;
            return;
        }
        let indent = " ".repeat(p.indent);
        let next = match blk {
            Block::Check(_) => format!("{indent}- [ ] "),
            Block::Number => {
                let n: u64 = line[p.indent..]
                    .split('.')
                    .next()
                    .and_then(|d| d.parse().ok())
                    .unwrap_or(0);
                format!("{indent}{}. ", n + 1)
            }
            Block::Quote => "> ".to_string(),
            _ => format!("{indent}{}", &line[p.indent..p.indent + p.len]),
        };
        self.insert(&format!("\n{next}"));
    }

    /// Retour arrière ; au début d'un élément de liste, retire la puce
    pub fn backspace(&mut self, word: bool) {
        if let Some((a, b)) = self.selection() {
            self.checkpoint(Edit::Other);
            return self.splice(a, b, "");
        }
        if self.cursor == 0 {
            return;
        }
        let (a, _) = self.paragraph(self.cursor);
        if let (_, Some(p)) = self.line_block(a)
            && self.cursor == a + p.indent + p.len
        {
            self.checkpoint(Edit::Other);
            // Indenté : remonte d'un niveau ; sinon la puce disparaît
            let cut = if p.indent >= 2 {
                (a, a + 2)
            } else {
                (a, a + p.indent + p.len)
            };
            self.text.replace_range(cut.0..cut.1, "");
            self.cursor -= cut.1 - cut.0;
            self.anchor = self.cursor;
            return;
        }
        let from = if word {
            self.word_left(self.cursor)
        } else {
            prev_char(&self.text, self.cursor)
        };
        self.checkpoint(Edit::Erase);
        self.splice(from, self.cursor, "");
    }

    pub fn delete(&mut self, word: bool) {
        if let Some((a, b)) = self.selection() {
            self.checkpoint(Edit::Other);
            return self.splice(a, b, "");
        }
        if self.cursor >= self.text.len() {
            return;
        }
        let to = if word {
            self.word_right(self.cursor)
        } else {
            next_char(&self.text, self.cursor)
        };
        self.checkpoint(Edit::Erase);
        let at = self.cursor;
        self.splice(at, to, "");
        self.cursor = at;
        self.anchor = at;
    }

    /// Tabulation : dans une liste, indente (ou désindente) les éléments touchés
    pub fn indent(&mut self, out: bool) {
        let s0 = self.selection().map_or(self.cursor, |(a, _)| a);
        let (a, _) = self.paragraph(s0);
        if self.line_block(a).1.is_none() {
            if !out {
                self.insert("    ");
            }
            return;
        }
        self.checkpoint(Edit::Other);
        // De la fin vers le début : les positions plus haut restent justes
        for (p, b) in self.touched().into_iter().rev() {
            let line = &self.text[p..b];
            if block(line, false, false).1.is_none() {
                continue;
            }
            if out {
                let n = (line.len() - line.trim_start_matches(' ').len()).min(2);
                self.text.replace_range(p..p + n, "");
                for c in [&mut self.cursor, &mut self.anchor] {
                    if *c >= p {
                        *c -= n.min(*c - p);
                    }
                }
            } else {
                self.text.insert_str(p, "  ");
                for c in [&mut self.cursor, &mut self.anchor] {
                    if *c >= p {
                        *c += 2;
                    }
                }
            }
        }
    }

    /// Case cochée ou décochée (clic sur la case du paragraphe qui commence en `a`)
    pub fn toggle_check(&mut self, a: usize) {
        let (_, b) = self.paragraph(a);
        let line = &self.text[a..b];
        let indent = line.len() - line.trim_start_matches(' ').len();
        let at = a + indent + 3;
        let now = match self.text.get(at..at + 1) {
            Some(" ") => "x",
            Some("x") | Some("X") => " ",
            _ => return,
        };
        self.checkpoint(Edit::Other);
        let (c, an) = (self.cursor, self.anchor);
        self.text.replace_range(at..at + 1, now);
        (self.cursor, self.anchor) = (c, an);
    }

    /// Mise en forme depuis la barre d'outils ou un raccourci
    pub fn format(&mut self, f: Format) {
        match f {
            Format::Bold => self.wrap("**"),
            Format::Italic => self.wrap("*"),
            Format::Strike => self.wrap("~~"),
            Format::Code => self.wrap("`"),
            Format::Heading => self.cycle_heading(),
            Format::Bullet => self.toggle_prefix("- "),
            Format::Check => self.toggle_prefix("- [ ] "),
        }
    }

    /// Entoure la sélection (ou le mot sous le curseur) de `d`, ou retire `d`
    fn wrap(&mut self, d: &str) {
        let (a, b) = match self.selection() {
            Some(s) => s,
            None => {
                let (a, b) = (self.word_left_strict(), self.word_right_strict());
                if a == b {
                    // Rien à entourer : la paire, curseur au milieu
                    self.checkpoint(Edit::Other);
                    let at = self.cursor;
                    self.splice(at, at, &format!("{d}{d}"));
                    self.cursor = at + d.len();
                    self.anchor = self.cursor;
                    return;
                }
                (a, b)
            }
        };
        self.checkpoint(Edit::Other);
        let n = d.len();
        let wrapped =
            a >= n && self.text.get(a - n..a) == Some(d) && self.text.get(b..b + n) == Some(d);
        if wrapped {
            self.text.replace_range(b..b + n, "");
            self.text.replace_range(a - n..a, "");
            (self.anchor, self.cursor) = (a - n, b - n);
        } else {
            self.text.insert_str(b, d);
            self.text.insert_str(a, d);
            (self.anchor, self.cursor) = (a + n, b + n);
        }
    }

    /// Paragraphes touchés par la sélection : (début, fin) de chacun
    fn touched(&self) -> Vec<(usize, usize)> {
        let (s0, s1) = self.selection().unwrap_or((self.cursor, self.cursor));
        let mut out = Vec::new();
        let mut a = self.paragraph(s0).0;
        loop {
            let (_, b) = self.paragraph(a);
            out.push((a, b));
            if b >= s1 || b >= self.text.len() {
                break;
            }
            a = b + 1;
        }
        out
    }

    /// Met ou retire un début de liste sur les paragraphes touchés
    fn toggle_prefix(&mut self, marker: &str) {
        self.checkpoint(Edit::Other);
        let paras = self.touched();
        let all = paras.iter().all(|&(a, b)| {
            let l = &self.text[a..b];
            l.trim_start_matches(' ').starts_with(marker)
                && (marker != "- " || !l.trim_start_matches(' ').starts_with("- ["))
        });
        // De la fin vers le début : les positions plus haut restent justes
        let mut delta_c = 0isize;
        let mut delta_a = 0isize;
        for &(a, b) in paras.iter().rev() {
            let line = self.text[a..b].to_string();
            let (_, prefix) = block(&line, false, false);
            let indent = prefix.map_or(0, |p| p.indent);
            let old = prefix.map_or(0, |p| p.len);
            let (cut, new) = if all {
                (indent + old, " ".repeat(indent))
            } else {
                (indent + old, format!("{}{marker}", " ".repeat(indent)))
            };
            let d = new.len() as isize - cut as isize;
            self.text.replace_range(a..a + cut, &new);
            if self.cursor >= a {
                delta_c += d;
            }
            if self.anchor >= a {
                delta_a += d;
            }
        }
        self.cursor = (self.cursor as isize + delta_c).max(0) as usize;
        self.anchor = (self.anchor as isize + delta_a).max(0) as usize;
        self.clamp();
    }

    /// Intertitre : aucun → # → ## → ### → aucun
    fn cycle_heading(&mut self) {
        self.checkpoint(Edit::Other);
        let (a, b) = self.paragraph(self.cursor);
        let line = &self.text[a..b];
        let hashes = line.bytes().take_while(|c| *c == b'#').count();
        let old = if hashes > 0 && line[hashes..].starts_with(' ') {
            hashes + 1
        } else {
            0
        };
        let new = match hashes {
            _ if old == 0 => "# ".to_string(),
            1 | 2 => format!("{} ", "#".repeat(hashes + 1)),
            _ => String::new(),
        };
        self.text.replace_range(a..a + old, &new);
        let d = new.len() as isize - old as isize;
        self.cursor = (self.cursor as isize + d).max(a as isize) as usize;
        self.anchor = (self.anchor as isize + d).max(a as isize) as usize;
        self.clamp();
    }

    fn clamp(&mut self) {
        let n = self.text.len();
        self.cursor = floor_char(&self.text, self.cursor.min(n));
        self.anchor = floor_char(&self.text, self.anchor.min(n));
    }

    // --- Déplacements ---

    /// Place le curseur ; `select` étend la sélection
    pub fn move_to(&mut self, pos: usize, select: bool) {
        self.cursor = self.snap(pos.min(self.text.len()), pos < self.cursor);
        if !select {
            self.anchor = self.cursor;
        }
        self.last = None;
    }

    /// Hors du début de liste caché (puce, case)
    fn snap(&self, pos: usize, backward: bool) -> usize {
        let (a, _) = self.paragraph(pos);
        if let (_, Some(p)) = self.line_block(a) {
            let end = a + p.indent + p.len;
            if pos > a + p.indent && pos < end {
                return if backward { a } else { end };
            }
            if pos > a && pos <= a + p.indent && p.indent > 0 {
                return if backward { a } else { end };
            }
        }
        pos
    }

    pub fn left(&mut self, word: bool, select: bool) {
        if !select && let Some((a, _)) = self.selection() {
            return self.move_to(a, false);
        }
        let to = if word {
            self.word_left(self.cursor)
        } else {
            prev_char(&self.text, self.cursor)
        };
        self.goal_x = None;
        self.move_to(to, select);
    }

    pub fn right(&mut self, word: bool, select: bool) {
        if !select && let Some((_, b)) = self.selection() {
            return self.move_to(b, false);
        }
        let to = if word {
            self.word_right(self.cursor)
        } else {
            next_char(&self.text, self.cursor)
        };
        self.goal_x = None;
        self.move_to(to, select);
    }

    pub fn select_all(&mut self) {
        self.anchor = 0;
        self.cursor = self.text.len();
    }

    /// Sélectionne le mot sous `pos` (double-clic)
    pub fn select_word(&mut self, pos: usize) {
        self.cursor = pos;
        let (a, b) = (self.word_left_strict(), self.word_right_strict());
        self.anchor = a;
        self.cursor = b;
    }

    /// Sélectionne le paragraphe sous `pos` (triple-clic)
    pub fn select_paragraph(&mut self, pos: usize) {
        let (a, b) = self.paragraph(pos);
        self.anchor = a;
        self.cursor = b;
    }

    /// Début du mot à gauche (Ctrl+←) : saute les espaces puis le mot ;
    /// s'arrête aux retours à la ligne
    fn word_left(&self, from: usize) -> usize {
        let before: Vec<(usize, char)> = self.text[..from].char_indices().collect();
        let mut i = before.len();
        if i > 0 && before[i - 1].1 == '\n' {
            return before[i - 1].0;
        }
        while i > 0 && !before[i - 1].1.is_alphanumeric() && before[i - 1].1 != '\n' {
            i -= 1;
        }
        while i > 0 && before[i - 1].1.is_alphanumeric() {
            i -= 1;
        }
        before.get(i).map_or(from, |c| c.0)
    }

    /// Fin du mot à droite (Ctrl+→)
    fn word_right(&self, from: usize) -> usize {
        let rest = &self.text[from..];
        if rest.starts_with('\n') {
            return from + 1;
        }
        let mut chars = rest.char_indices().peekable();
        while chars
            .next_if(|(_, c)| !c.is_alphanumeric() && *c != '\n')
            .is_some()
        {}
        while chars.next_if(|(_, c)| c.is_alphanumeric()).is_some() {}
        from + chars.peek().map_or(rest.len(), |(i, _)| *i)
    }

    /// Bornes du mot qui touche le curseur (vides entre deux espaces)
    fn word_left_strict(&self) -> usize {
        self.text[..self.cursor]
            .char_indices()
            .rev()
            .take_while(|(_, c)| c.is_alphanumeric() || *c == '\'' || *c == '-')
            .last()
            .map_or(self.cursor, |(i, _)| i)
    }

    fn word_right_strict(&self) -> usize {
        self.text[self.cursor..]
            .char_indices()
            .take_while(|(_, c)| c.is_alphanumeric() || *c == '\'' || *c == '-')
            .last()
            .map_or(self.cursor, |(i, c)| self.cursor + i + c.len_utf8())
    }
}

fn prev_char(s: &str, i: usize) -> usize {
    s[..i].char_indices().next_back().map_or(0, |(k, _)| k)
}

fn next_char(s: &str, i: usize) -> usize {
    s[i..].chars().next().map_or(i, |c| i + c.len_utf8())
}

fn floor_char(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Titre d'une note : sa première ligne non vide, sans la syntaxe
pub fn title(text: &str) -> String {
    text.lines()
        .map(strip)
        .find(|l| !l.is_empty())
        .unwrap_or_default()
}

/// Aperçu : les lignes suivantes, à plat
pub fn preview(text: &str) -> String {
    let mut lines = text.lines().map(strip).filter(|l| !l.is_empty());
    lines.next();
    let mut out = String::new();
    for l in lines {
        if out.len() > 160 {
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&l);
    }
    out
}

/// Ligne sans syntaxe Markdown (titres, listes, emphase)
fn strip(line: &str) -> String {
    let (blk, prefix) = block(line, false, false);
    if blk == Block::Fence {
        return String::new();
    }
    let styles = inline(line, blk, prefix);
    let out: String = line
        .chars()
        .zip(styles)
        .filter(|(_, s)| !s.marker && !s.hidden)
        .map(|(c, _)| c)
        .collect();
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Mono;
    impl Metrics for Mono {
        fn advance(&self, _: &Style, _: Block, _: char, _: f32) -> f32 {
            10.0
        }
    }

    const SZ: Sizes = Sizes {
        body: 10.0,
        code: 10.0,
        title: 10.0,
        h1: 10.0,
        h2: 10.0,
        h3: 10.0,
        indent: 20.0,
        widget: 20.0,
    };

    #[test]
    fn blocks() {
        assert_eq!(block("Courses", true, false).0, Block::Title);
        assert_eq!(block("# Titre", false, false).0, Block::Heading(1));
        assert_eq!(block("#hashtag", false, false).0, Block::Normal);
        assert_eq!(
            block("  - [x] fait", false, false),
            (Block::Check(true), Some(Prefix { indent: 2, len: 6 }))
        );
        assert_eq!(
            block("12. douze", false, false),
            (Block::Number, Some(Prefix { indent: 0, len: 4 }))
        );
        assert_eq!(block("x", false, true).0, Block::Code);
    }

    #[test]
    fn emphasis() {
        let s = inline("a **gras** et *it* `c*d*`", Block::Normal, None);
        let bold: String = "a **gras** et *it* `c*d*`"
            .chars()
            .zip(&s)
            .filter(|(_, s)| s.bold && !s.marker)
            .map(|(c, _)| c)
            .collect();
        assert_eq!(bold, "gras");
        assert!(s[14].marker && s[15].italic && !s[21].italic && s[21].code);
        // snake_case reste tel quel
        assert!(
            inline("nom_de_fichier", Block::Normal, None)
                .iter()
                .all(|s| !s.marker)
        );
    }

    #[test]
    fn wraps_and_maps() {
        let lay = layout("titre\nun deux trois", &Mono, &SZ, 55.0);
        assert_eq!(lay.lines.len(), 4);
        assert_eq!(lay.lines[1].start, 6);
        assert_eq!(lay.lines[2].start, 9);
        // Fin d'une ligne coupée : avant l'espace
        assert_eq!(lay.lines[1].byte_at(1000.0), 8);
        assert_eq!(lay.line_of(9), 2);
        assert_eq!(lay.line_of(20), 3);
        assert_eq!(lay.byte_at(0.0, lay.lines[3].y + 1.0), 14);
    }

    #[test]
    fn lists_continue_and_end() {
        let mut e = Editor::new("Courses\n- [ ] pain".into());
        e.newline();
        assert_eq!(e.text, "Courses\n- [ ] pain\n- [ ] ");
        e.newline();
        assert_eq!(e.text, "Courses\n- [ ] pain\n");
        let mut e = Editor::new("9. neuf".into());
        e.newline();
        assert_eq!(e.text, "9. neuf\n10. ");
        e.undo();
        assert_eq!(e.text, "9. neuf");
    }

    #[test]
    fn formatting() {
        let mut e = Editor::new("un mot".into());
        e.move_to(4, false);
        e.format(Format::Bold);
        assert_eq!(e.text, "un **mot**");
        e.format(Format::Bold);
        assert_eq!(e.text, "un mot");
        e.select_all();
        e.format(Format::Check);
        assert_eq!(e.text, "- [ ] un mot");
        e.format(Format::Check);
        assert_eq!(e.text, "un mot");
        e.format(Format::Heading);
        e.format(Format::Heading);
        assert_eq!(e.text, "## un mot");
        e.toggle_check(0);
        assert_eq!(e.text, "## un mot");
        let mut e = Editor::new("- [ ] a".into());
        e.toggle_check(0);
        assert_eq!(e.text, "- [x] a");
    }

    #[test]
    fn typing_undo_groups() {
        let mut e = Editor::new(String::new());
        for c in "bonjour".chars() {
            e.insert(&c.to_string());
        }
        e.backspace(true);
        assert_eq!(e.text, "");
        e.undo();
        assert_eq!(e.text, "bonjour");
        e.undo();
        assert_eq!(e.text, "");
        e.redo();
        assert_eq!(e.text, "bonjour");
    }

    #[test]
    fn bullets_snap_and_backspace() {
        let mut e = Editor::new("- a".into());
        e.move_to(0, false);
        e.move_to(1, false);
        assert_eq!(e.cursor, 2);
        e.backspace(false);
        assert_eq!(e.text, "a");
        let mut e = Editor::new("- a\n  - b".into());
        e.move_to(8, false);
        e.indent(true);
        assert_eq!(e.text, "- a\n- b");
        assert_eq!(e.cursor, 6);
        e.indent(false);
        assert_eq!(e.text, "- a\n  - b");
    }

    #[test]
    fn titles() {
        assert_eq!(title("\n# **Projet** odysseus\nsuite"), "Projet odysseus");
        assert_eq!(preview("Titre\n- [ ] pain\n- lait"), "pain lait");
    }
}
