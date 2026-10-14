//! Thème visuel du bureau : palettes au choix, tailles et polices communes. Toutes les tailles sont en pixels logiques
//! (avant mise à l'échelle de l'écran).

use std::{
    ffi::CString,
    os::{
        fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd},
        unix::ffi::OsStrExt,
    },
    path::PathBuf,
    sync::{
        Once,
        atomic::{AtomicUsize, Ordering},
    },
};

/// Couleur RGBA, composantes 0.0–1.0 (alpha non prémultiplié).
#[derive(Clone, Copy, Debug)]
pub struct Rgba {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Rgba {
    pub const fn hex(rgb: u32, a: f32) -> Self {
        Self {
            r: ((rgb >> 16) & 0xff) as f32 / 255.0,
            g: ((rgb >> 8) & 0xff) as f32 / 255.0,
            b: (rgb & 0xff) as f32 / 255.0,
            a,
        }
    }

    pub const fn with_alpha(self, a: f32) -> Self {
        Self { a, ..self }
    }

    /// Mélange vers `other` : 0 donne `self`, 1 donne `other`
    pub fn mix(self, other: Rgba, t: f32) -> Self {
        let m = |a: f32, b: f32| a + (b - a) * t;
        Self {
            r: m(self.r, other.r),
            g: m(self.g, other.g),
            b: m(self.b, other.b),
            a: m(self.a, other.a),
        }
    }
}

/// Palette d'un thème. Les applications la lisent par [`palette()`] à chaque
/// dessin : un changement de thème s'applique au dessin suivant.
pub struct Palette {
    /// Nom dans le fichier de réglage, et nom affiché
    pub id: &'static str,
    pub name: &'static str,
    pub background: Rgba,
    /// Fond de la barre du haut
    pub surface: Rgba,
    pub text: Rgba,
    pub text_dim: Rgba,
    pub accent: Rgba,
    /// Texte posé sur l'accent
    pub on_accent: Rgba,
    pub urgent: Rgba,
    pub danger: Rgba,
    pub success: Rgba,
    /// Fond des éléments discrets (bureaux inactifs…)
    pub subtle: Rgba,
    /// Panneaux flottants (centre de contrôle, notifications, verrou)
    pub panel: Rgba,
    /// Fenêtres : barre latérale et contenu (Fichiers)
    pub sidebar: Rgba,
    pub content: Rgba,
    /// Encre des voiles et liserés : blanc sur un thème sombre, noir sur un clair
    pub ink: u32,
}

impl Palette {
    /// Voile ou liseré de l'encre du thème, à l'opacité `a`
    pub const fn ink(&self, a: f32) -> Rgba {
        Rgba::hex(self.ink, a)
    }
    pub const fn panel_border(&self) -> Rgba {
        self.ink(0.10)
    }
    /// Tuile dans un panneau, et tuile survolée
    pub const fn tile(&self) -> Rgba {
        self.ink(0.07)
    }
    pub const fn tile_hover(&self) -> Rgba {
        self.ink(0.12)
    }
    /// Texte secondaire, entre le texte et le texte atténué
    pub fn text_soft(&self) -> Rgba {
        self.text.mix(self.text_dim, 0.35)
    }
    /// Accent survolé : un peu plus vers l'encre (plus clair sur un thème sombre)
    pub fn accent_hover(&self) -> Rgba {
        self.accent.mix(self.ink(1.0), 0.18)
    }
    /// Liseré intérieur, très discret, qui détache la fenêtre du fond
    pub const fn window_border(&self) -> Rgba {
        self.ink(0.09)
    }
}

pub const THEMES: [Palette; 3] = [
    Palette {
        id: "bleue",
        name: "Odyssée bleue",
        background: Rgba::hex(0x1e1e2e, 1.0),
        surface: Rgba::hex(0x181825, 0.88),
        text: Rgba::hex(0xe6e6ef, 1.0),
        text_dim: Rgba::hex(0x9a9ab0, 1.0),
        accent: Rgba::hex(0x8aadf4, 1.0),
        on_accent: Rgba::hex(0x1e1e2e, 1.0),
        urgent: Rgba::hex(0xf38ba8, 1.0),
        danger: Rgba::hex(0xf38ba8, 1.0),
        success: Rgba::hex(0xa6e3a1, 1.0),
        subtle: Rgba::hex(0x313244, 0.9),
        panel: Rgba::hex(0x1e1e2e, 0.94),
        sidebar: Rgba::hex(0x181825, 1.0),
        content: Rgba::hex(0x1e1e2e, 1.0),
        ink: 0xffffff,
    },
    // Orange argile, gris chauds, ivoire
    Palette {
        id: "sombre",
        name: "Odyssée sombre",
        background: Rgba::hex(0x262624, 1.0),
        surface: Rgba::hex(0x1f1e1d, 0.90),
        text: Rgba::hex(0xf5f4ef, 1.0),
        text_dim: Rgba::hex(0xa6a39a, 1.0),
        accent: Rgba::hex(0xd97757, 1.0),
        on_accent: Rgba::hex(0xffffff, 1.0),
        urgent: Rgba::hex(0xd97757, 1.0),
        danger: Rgba::hex(0xff6b80, 1.0),
        success: Rgba::hex(0x4eba65, 1.0),
        subtle: Rgba::hex(0x3a3936, 0.9),
        panel: Rgba::hex(0x262624, 0.95),
        sidebar: Rgba::hex(0x1f1e1d, 1.0),
        content: Rgba::hex(0x262624, 1.0),
        ink: 0xffffff,
    },
    Palette {
        id: "claire",
        name: "Odyssée claire",
        background: Rgba::hex(0xfaf9f5, 1.0),
        surface: Rgba::hex(0xf0eee6, 0.92),
        text: Rgba::hex(0x141413, 1.0),
        text_dim: Rgba::hex(0x6b6a64, 1.0),
        accent: Rgba::hex(0xc96442, 1.0),
        on_accent: Rgba::hex(0xffffff, 1.0),
        urgent: Rgba::hex(0xc96442, 1.0),
        danger: Rgba::hex(0xab2b3f, 1.0),
        success: Rgba::hex(0x2c7a39, 1.0),
        subtle: Rgba::hex(0xe3e0d5, 0.95),
        panel: Rgba::hex(0xfaf9f5, 0.96),
        sidebar: Rgba::hex(0xf0eee6, 1.0),
        content: Rgba::hex(0xfaf9f5, 1.0),
        ink: 0x141413,
    },
];

static CURRENT: AtomicUsize = AtomicUsize::new(0);
static LOADED: Once = Once::new();

/// Palette du thème choisi (lu dans `~/.config/odysseus/theme` au premier appel,
/// relu par [`reload()`])
pub fn palette() -> &'static Palette {
    LOADED.call_once(|| {
        reload();
    });
    &THEMES[CURRENT.load(Ordering::Relaxed)]
}

/// Fichier qui contient le nom du thème choisi
pub fn config_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("odysseus/theme"))
}

/// Relit le thème choisi ; vrai s'il a changé.
pub fn reload() -> bool {
    let id = config_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();
    let index = THEMES.iter().position(|t| t.id == id.trim()).unwrap_or(0);
    CURRENT.swap(index, Ordering::Relaxed) != index
}

/// Choisit un thème pour toute la session (les applications ouvertes le suivent).
pub fn set(id: &str) -> std::io::Result<()> {
    let path = config_path().ok_or_else(|| std::io::Error::other("HOME inconnu"))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Écrit à côté puis renomme : un lecteur ne voit jamais un fichier à moitié écrit
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, id)?;
    std::fs::rename(&tmp, &path)?;
    reload();
    Ok(())
}

/// Surveillance du réglage (inotify) : le descripteur devient lisible quand le
/// thème change ; appeler alors [`drain()`] puis [`reload()`].
pub fn watch() -> Option<OwnedFd> {
    let dir = config_path()?.parent()?.to_path_buf();
    std::fs::create_dir_all(&dir).ok()?;
    let c_dir = CString::new(dir.as_os_str().as_bytes()).ok()?;
    unsafe {
        let fd = libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC);
        if fd < 0 {
            return None;
        }
        let fd = OwnedFd::from_raw_fd(fd);
        let mask = libc::IN_CLOSE_WRITE | libc::IN_MOVED_TO | libc::IN_DELETE;
        (libc::inotify_add_watch(fd.as_raw_fd(), c_dir.as_ptr(), mask) >= 0).then_some(fd)
    }
}

/// Vide les événements en attente du descripteur de [`watch()`]
pub fn drain(fd: BorrowedFd) {
    let mut buf = [0u8; 1024];
    while unsafe { libc::read(fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) } > 0 {}
}

/// Grille d'espacement : tout est multiple de 4.
pub const SPACE: u32 = 4;

/// Barre du haut
pub const BAR_HEIGHT: u32 = 8 * SPACE;
pub const BAR_PADDING_X: u32 = 4 * SPACE;

/// Pastilles des bureaux dans la barre
pub const PILL_HEIGHT: u32 = 6 * SPACE;
pub const PILL_PADDING_X: u32 = 2 * SPACE;
pub const PILL_GAP: u32 = 2 * SPACE;
pub const PILL_RADIUS: f32 = 3.0 * SPACE as f32;

/// Fenêtres (compositeur)
pub const WINDOW_RADIUS: f32 = 3.0 * SPACE as f32;
pub const SHADOW_BLUR: i32 = 9 * SPACE as i32;
pub const SHADOW_OFFSET_Y: i32 = 3 * SPACE as i32;
pub const SHADOW_ALPHA_FOCUSED: f32 = 0.55;
pub const SHADOW_ALPHA: f32 = 0.30;

/// Typographie
pub const FONT_REGULAR: &str = "/usr/share/fonts/inter/Inter-Medium.ttf";
pub const FONT_BOLD: &str = "/usr/share/fonts/inter/Inter-SemiBold.ttf";
pub const FONT_SIZE: f32 = 14.0;

/// Icônes : police Lucide (ISC), un glyphe par icône
pub const FONT_ICONS: &str = "/usr/share/fonts/lucide/lucide.ttf";

/// Panneaux flottants : coins des panneaux et des tuiles qu'ils contiennent
pub const PANEL_RADIUS: f32 = 4.0 * SPACE as f32;
pub const TILE_RADIUS: f32 = 3.0 * SPACE as f32;
/// Écart entre un panneau et le bord de l'écran / la barre
pub const PANEL_MARGIN: u32 = 2 * SPACE;
pub const FONT_SMALL: f32 = 12.0;
pub const FONT_TITLE: f32 = 15.0;

/// Glyphes de la police d'icônes
pub mod icon {
    pub const WIFI: char = '\u{e1ae}';
    pub const WIFI_OFF: char = '\u{e1af}';
    pub const WIFI_LOW: char = '\u{e5f8}';
    pub const WIFI_HIGH: char = '\u{e5f7}';
    pub const WIFI_ZERO: char = '\u{e5f9}';
    pub const ETHERNET: char = '\u{e620}';
    pub const NETWORK: char = '\u{e125}';
    pub const BLUETOOTH: char = '\u{e05c}';
    pub const BLUETOOTH_OFF: char = '\u{e1b9}';
    pub const BLUETOOTH_CONNECTED: char = '\u{e1b8}';
    pub const VOLUME_MUTE: char = '\u{e1ac}';
    pub const VOLUME_0: char = '\u{e1a9}';
    pub const VOLUME_1: char = '\u{e1aa}';
    pub const VOLUME_2: char = '\u{e1ab}';
    pub const MOON: char = '\u{e11e}';
    pub const BELL: char = '\u{e059}';
    pub const BELL_OFF: char = '\u{e05a}';
    pub const LOCK: char = '\u{e10b}';
    pub const POWER: char = '\u{e140}';
    pub const RESTART: char = '\u{e148}';
    pub const CONTROLS: char = '\u{e29a}';
    pub const CLOSE: char = '\u{e1b2}';
    pub const CHEVRON_RIGHT: char = '\u{e06f}';
    pub const CHEVRON_LEFT: char = '\u{e06e}';
    pub const CHEVRON_UP: char = '\u{e070}';
    pub const CHEVRON_DOWN: char = '\u{e06d}';
    pub const COPY: char = '\u{e09e}';
    pub const EXTERNAL_LINK: char = '\u{e0b9}';
    pub const LINK: char = '\u{e102}';
    pub const CHECK: char = '\u{e06c}';
    pub const HEADPHONES: char = '\u{e0f1}';
    pub const SPEAKER: char = '\u{e166}';
    pub const MONITOR: char = '\u{e11d}';
    pub const KEYBOARD: char = '\u{e284}';
    pub const MOUSE: char = '\u{e28e}';
    pub const USER: char = '\u{e461}';
    pub const ARROW_RIGHT: char = '\u{e07a}';
    pub const LOADER: char = '\u{e109}';
    pub const BLUETOOTH_SEARCH: char = '\u{e1ba}';
    pub const HARD_DRIVE: char = '\u{e0ed}';
    pub const USB: char = '\u{e356}';
    pub const EJECT: char = '\u{e733}';
    pub const SUN: char = '\u{e178}';
    pub const SUN_MOON: char = '\u{e2b2}';
    pub const PALETTE: char = '\u{e1dd}';
    pub const SLEEP: char = '\u{e410}';
    pub const APP: char = '\u{e426}';
    pub const ROTATE_CW: char = '\u{e149}';
    pub const ROTATE_CCW: char = '\u{e148}';
    pub const CROP: char = '\u{e0ab}';
    pub const INFO: char = '\u{e0f9}';
    pub const TRASH: char = '\u{e18e}';
    pub const ZOOM_IN: char = '\u{e1b6}';
    pub const ZOOM_OUT: char = '\u{e1b7}';
    pub const MAXIMIZE: char = '\u{e112}';
    pub const SAVE: char = '\u{e14d}';
    pub const IMAGE: char = '\u{e0f6}';
    pub const UNDO: char = '\u{e2a1}';
    pub const ACTIVITY: char = '\u{e038}';
    pub const GAUGE: char = '\u{e1bf}';
    pub const CPU: char = '\u{e0a9}';
    pub const MEMORY: char = '\u{e445}';
    pub const GPU: char = '\u{e66a}';
    pub const THERMOMETER: char = '\u{e186}';
    pub const ZAP: char = '\u{e1b4}';
    pub const FAN: char = '\u{e379}';
    pub const ARROW_DOWN: char = '\u{e042}';
    pub const ARROW_UP: char = '\u{e04a}';
}
