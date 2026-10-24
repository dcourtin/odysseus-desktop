//! Accord des applis tierces avec le bureau : elles n'utilisent pas le dessin
//! d'odysseus, mais suivent quelques conventions qu'on peut leur donner.
//!
//! - Police : Inter, celle du bureau, devient la police « sans-serif » et
//!   « system-ui » de fontconfig (`~/.config/fontconfig/conf.d/50-odysseus.conf`)
//!   et la police de GTK (`gtk-font-name`) : Firefox, Obsidian, dialogues GTK.
//! - Couleurs : `odysseus.css` (GTK 3 et 4) tiré de la palette du thème, y
//!   compris le thème personnalisé, importé en tête de `gtk.css` (les règles de
//!   l'utilisateur, après l'import, restent les siennes). Firefox, avec son
//!   thème « Système », prend ses couleurs de GTK.
//! - Clair ou sombre : `gtk-application-prefer-dark-theme`.
//! - Firefox : avec le thème GTK Adwaita, il remplace les couleurs de GTK par
//!   une palette libadwaita figée ; la préférence
//!   `widget.gtk.libadwaita-colors.enabled` est mise à faux dans le `user.js`
//!   de chaque profil (le reste du fichier est gardé), pour qu'il prenne
//!   celles d'odysseus.
//!
//! Écrit à chaque changement de thème ([`crate::theme::set`]) et au démarrage
//! du démon de notifications ; une appli déjà ouverte le prend à sa relance
//! (le portail d'odysseus-notify lui signale tout de suite clair ou sombre et
//! l'accent).

use std::path::{Path, PathBuf};

use crate::theme::{self, Palette, Rgba};

const FONT_FAMILY: &str = "Inter";
/// 10 pt : la taille du texte des applis d'odysseus (13,5 px)
const FONT_SIZE: u32 = 10;

fn config_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
}

/// Écrit tout ce qui accorde les applis tierces au thème en vigueur
pub fn apply() {
    let Some(config) = config_dir() else {
        return;
    };
    let p = theme::palette();
    let _ = write_if_changed(
        &config.join("fontconfig/conf.d/50-odysseus.conf"),
        &fontconfig(),
    );
    for version in ["gtk-3.0", "gtk-4.0"] {
        let dir = config.join(version);
        let css = if version == "gtk-3.0" {
            gtk3_css(p)
        } else {
            gtk4_css(p)
        };
        let _ = write_if_changed(&dir.join("odysseus.css"), &css);
        let _ = ensure_import(&dir.join("gtk.css"));
        let _ = update_settings_ini(&dir.join("settings.ini"), p.ink == 0xffffff);
    }
    firefox_profiles(&config);
}

const FIREFOX_PREF: &str = "user_pref(\"widget.gtk.libadwaita-colors.enabled\", false); // odysseus : couleurs du thème du bureau";

/// `user.js` avec la préférence d'odysseus (None : déjà là)
fn with_firefox_pref(existing: &str) -> Option<String> {
    if existing.contains("widget.gtk.libadwaita-colors.enabled") {
        return None;
    }
    let mut text = existing.to_string();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(FIREFOX_PREF);
    text.push('\n');
    Some(text)
}

/// Profils Firefox (dossiers qui ont un prefs.js), anciens et nouveaux emplacements
fn firefox_profiles(config: &Path) {
    let mut roots = vec![config.join("mozilla/firefox")];
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(home).join(".mozilla/firefox"));
    }
    for root in roots {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        for dir in entries.flatten().map(|e| e.path()) {
            if !dir.join("prefs.js").exists() {
                continue;
            }
            let path = dir.join("user.js");
            let existing = std::fs::read_to_string(&path).unwrap_or_default();
            if let Some(text) = with_firefox_pref(&existing) {
                let _ = std::fs::write(&path, text);
            }
        }
    }
}

fn write_if_changed(path: &Path, text: &str) -> std::io::Result<()> {
    if std::fs::read_to_string(path).is_ok_and(|t| t == text) {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(tmp, path)
}

fn fontconfig() -> String {
    let alias = |family: &str| {
        format!(
            "  <alias binding=\"same\">\n    <family>{family}</family>\n    <prefer><family>{FONT_FAMILY}</family></prefer>\n  </alias>\n"
        )
    };
    format!(
        "<?xml version=\"1.0\"?>\n<!DOCTYPE fontconfig SYSTEM \"urn:fontconfig:fonts.dtd\">\n\
         <!-- Écrit par odysseus : Inter, la police du bureau, pour les applis qui\n     \
         demandent la police du système (Firefox, Obsidian, GTK). Supprimer ce\n     \
         fichier rend la main aux polices par défaut. -->\n<fontconfig>\n{}{}</fontconfig>\n",
        alias("sans-serif"),
        alias("system-ui"),
    )
}

/// Ligne d'import d'`odysseus.css`, en tête de `gtk.css` (GTK veut les
/// imports avant toute règle) ; le reste du fichier est gardé
const IMPORT: &str = "@import url(\"odysseus.css\"); /* thème d'odysseus */";

fn with_import(existing: &str) -> Option<String> {
    if existing.lines().any(|l| l.trim() == IMPORT) {
        return None;
    }
    Some(if existing.trim().is_empty() {
        format!("{IMPORT}\n/* Vos propres règles GTK ici : elles passent après celles d'odysseus. */\n")
    } else {
        format!("{IMPORT}\n{existing}")
    })
}

fn ensure_import(path: &Path) -> std::io::Result<()> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    match with_import(&existing) {
        Some(text) => write_if_changed(path, &text),
        None => Ok(()),
    }
}

/// Clés de `settings.ini` tenues par odysseus ; les autres restent
fn settings_ini(existing: &str, dark: bool) -> String {
    let ours = [
        ("gtk-application-prefer-dark-theme", dark.to_string()),
        ("gtk-font-name", format!("{FONT_FAMILY} {FONT_SIZE}")),
    ];
    let mut lines: Vec<String> = existing
        .lines()
        .filter(|l| {
            let key = l.split('=').next().unwrap_or("").trim();
            !ours.iter().any(|(k, _)| *k == key)
        })
        .map(String::from)
        .collect();
    let at = match lines.iter().position(|l| l.trim() == "[Settings]") {
        Some(i) => i + 1,
        None => {
            lines.insert(0, "[Settings]".into());
            1
        }
    };
    for (i, (k, v)) in ours.iter().enumerate() {
        lines.insert(at + i, format!("{k}={v}"));
    }
    lines.join("\n") + "\n"
}

fn update_settings_ini(path: &Path, dark: bool) -> std::io::Result<()> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    write_if_changed(path, &settings_ini(&existing, dark))
}

fn css(c: Rgba) -> String {
    let v = |x: f32| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
    if c.a >= 0.999 {
        format!("#{:02x}{:02x}{:02x}", v(c.r), v(c.g), v(c.b))
    } else {
        format!("rgba({},{},{},{:.2})", v(c.r), v(c.g), v(c.b), c.a)
    }
}

/// Couleurs nommées : celles de GTK 3 (theme_*) et de libadwaita (GTK 4)
fn named_colors(p: &Palette) -> String {
    let bg = css(p.content);
    let fg = css(p.text);
    let pairs = [
        ("theme_bg_color", bg.clone()),
        ("theme_fg_color", fg.clone()),
        ("theme_base_color", bg.clone()),
        ("theme_text_color", fg.clone()),
        ("theme_selected_bg_color", css(p.accent)),
        ("theme_selected_fg_color", css(p.on_accent)),
        ("theme_unfocused_bg_color", bg.clone()),
        ("theme_unfocused_fg_color", fg.clone()),
        ("theme_unfocused_base_color", bg.clone()),
        ("theme_unfocused_text_color", fg.clone()),
        ("insensitive_fg_color", css(p.text_dim)),
        ("borders", css(p.panel_border())),
        ("accent_color", css(p.accent)),
        ("accent_bg_color", css(p.accent)),
        ("accent_fg_color", css(p.on_accent)),
        ("window_bg_color", bg.clone()),
        ("window_fg_color", fg.clone()),
        ("view_bg_color", bg.clone()),
        ("view_fg_color", fg.clone()),
        ("headerbar_bg_color", css(p.content.mix(p.surface.with_alpha(1.0), p.surface.a))),
        ("headerbar_fg_color", fg.clone()),
        ("sidebar_bg_color", css(p.sidebar)),
        ("sidebar_fg_color", fg.clone()),
        ("popover_bg_color", css(p.panel.with_alpha(1.0))),
        ("popover_fg_color", fg.clone()),
        ("dialog_bg_color", bg.clone()),
        ("dialog_fg_color", fg.clone()),
        ("card_bg_color", css(p.tile())),
        ("card_fg_color", fg),
        ("destructive_color", css(p.danger)),
        ("destructive_bg_color", css(p.danger)),
        ("success_color", css(p.success)),
    ];
    pairs
        .iter()
        .map(|(k, v)| format!("@define-color {k} {v};\n"))
        .collect()
}

fn header(p: &Palette) -> String {
    format!(
        "/* Écrit par odysseus d'après le thème « {} », à chaque changement de thème :\n   \
         ne pas modifier ici ; vos règles vont dans gtk.css, après la ligne @import. */\n\n",
        p.name
    )
}

fn gtk3_css(p: &Palette) -> String {
    let (bg, fg, dim) = (css(p.content), css(p.text), css(p.text_dim));
    let (acc, on) = (css(p.accent), css(p.on_accent));
    let border = css(p.panel_border());
    let (tile, tile_hover) = (css(p.tile()), css(p.tile_hover()));
    // Opaque : une barre translucide ne veut rien dire hors du compositeur
    let surface = css(p.content.mix(p.surface.with_alpha(1.0), p.surface.a));
    let sidebar = css(p.sidebar);
    let panel = css(p.panel.with_alpha(1.0));
    let danger = css(p.danger);
    let slider = css(p.ink(0.35));
    format!(
        "{header}{named}
window, .background, dialog, messagedialog {{ background-color: {bg}; color: {fg}; }}
.view, textview text, iconview, treeview, list, row, entry, spinbutton {{ background-color: {bg}; color: {fg}; }}
menubar, headerbar, .titlebar {{ background-image: none; background-color: {surface}; color: {fg}; border-color: {border}; box-shadow: none; }}
.sidebar, placessidebar, stacksidebar, .navigation-sidebar {{ background-color: {sidebar}; }}
menu, .menu, .context-menu, popover, popover.background, .popover, tooltip.background {{ background-color: {panel}; color: {fg}; border-color: {border}; }}
menuitem:hover, modelbutton:hover, .menuitem:hover {{ background-color: {acc}; color: {on}; }}
entry {{ border-color: {border}; box-shadow: none; }}
entry:focus {{ border-color: {acc}; box-shadow: inset 0 0 0 1px {acc}; }}
button {{ background-image: none; background-color: {tile}; color: {fg}; border-color: {border}; box-shadow: none; text-shadow: none; -gtk-icon-shadow: none; }}
button:hover {{ background-color: {tile_hover}; }}
button:checked, button.suggested-action {{ background-color: {acc}; color: {on}; border-color: {acc}; }}
button.destructive-action {{ background-color: {danger}; color: #ffffff; border-color: {danger}; }}
button:disabled, label:disabled, entry:disabled {{ color: {dim}; }}
*:selected, selection, row:selected, .view:selected, treeview:selected {{ background-color: {acc}; color: {on}; }}
check:checked, radio:checked, switch:checked {{ background-image: none; background-color: {acc}; border-color: {acc}; color: {on}; }}
progressbar progress, scale highlight, levelbar block.filled {{ background-image: none; background-color: {acc}; border-color: {acc}; }}
scrollbar slider {{ background-color: {slider}; }}
link, *:link {{ color: {acc}; }}
separator {{ background-color: {border}; }}
",
        header = header(p),
        named = named_colors(p),
    )
}

fn gtk4_css(p: &Palette) -> String {
    // libadwaita et le thème par défaut de GTK 4 se règlent par les couleurs nommées
    format!("{}{}", header(p), named_colors(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_once_and_first() {
        let text = with_import("label { color: red; }\n").unwrap();
        assert!(text.starts_with(IMPORT));
        assert!(text.ends_with("label { color: red; }\n"));
        assert!(with_import(&text).is_none());
        assert!(with_import("").unwrap().starts_with(IMPORT));
    }

    #[test]
    fn firefox_pref_once() {
        let t = with_firefox_pref("user_pref(\"a\", 1);").unwrap();
        assert!(t.starts_with("user_pref(\"a\", 1);\n"));
        assert!(t.ends_with("// odysseus : couleurs du thème du bureau\n"));
        assert!(with_firefox_pref(&t).is_none());
        // Réglée par l'utilisateur, quelle que soit la valeur : on n'y touche pas
        assert!(with_firefox_pref("user_pref(\"widget.gtk.libadwaita-colors.enabled\", true);").is_none());
    }

    #[test]
    fn settings_keep_others() {
        let out = settings_ini(
            "[Settings]\ngtk-application-prefer-dark-theme=false\ngtk-cursor-theme-name=Bibata\n",
            true,
        );
        assert!(out.contains("gtk-application-prefer-dark-theme=true\n"));
        assert!(out.contains("gtk-font-name=Inter 10\n"));
        assert!(out.contains("gtk-cursor-theme-name=Bibata\n"));
        assert_eq!(out.matches("prefer-dark").count(), 1);
        assert!(settings_ini("", false).starts_with("[Settings]\n"));
    }

    #[test]
    fn css_colors() {
        assert_eq!(css(Rgba::hex(0xd97757, 1.0)), "#d97757");
        assert_eq!(css(Rgba::hex(0xffffff, 0.1)), "rgba(255,255,255,0.10)");
        let p = theme::palette();
        let text = gtk3_css(p);
        assert!(text.contains(&format!("@define-color accent_color {};", css(p.accent))));
        assert!(text.contains("headerbar"));
    }
}
