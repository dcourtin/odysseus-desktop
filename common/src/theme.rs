//! Thème visuel unique du bureau. Toutes les tailles sont en pixels logiques
//! (avant mise à l'échelle de l'écran).

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
}

/// Palette
pub const BACKGROUND: Rgba = Rgba::hex(0x1e1e2e, 1.0);
pub const SURFACE: Rgba = Rgba::hex(0x181825, 0.88);
pub const TEXT: Rgba = Rgba::hex(0xe6e6ef, 1.0);
pub const TEXT_DIM: Rgba = Rgba::hex(0x9a9ab0, 1.0);
pub const ACCENT: Rgba = Rgba::hex(0x8aadf4, 1.0);
pub const URGENT: Rgba = Rgba::hex(0xf38ba8, 1.0);
/// Fond des éléments discrets (bureaux inactifs…)
pub const SUBTLE: Rgba = Rgba::hex(0x313244, 0.9);

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
/// Liseré intérieur, très discret, qui détache la fenêtre du fond
pub const WINDOW_BORDER: Rgba = Rgba::hex(0xffffff, 0.09);
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

/// Panneaux flottants (centre de contrôle, notifications, verrou)
pub const PANEL: Rgba = Rgba::hex(0x1e1e2e, 0.94);
pub const PANEL_BORDER: Rgba = Rgba::hex(0xffffff, 0.10);
pub const PANEL_RADIUS: f32 = 4.0 * SPACE as f32;
/// Tuile dans un panneau, et tuile survolée
pub const TILE: Rgba = Rgba::hex(0xffffff, 0.07);
pub const TILE_HOVER: Rgba = Rgba::hex(0xffffff, 0.12);
pub const TILE_RADIUS: f32 = 3.0 * SPACE as f32;
/// Écart entre un panneau et le bord de l'écran / la barre
pub const PANEL_MARGIN: u32 = 2 * SPACE;
pub const DANGER: Rgba = Rgba::hex(0xf38ba8, 1.0);
pub const SUCCESS: Rgba = Rgba::hex(0xa6e3a1, 1.0);
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
    pub const SLEEP: char = '\u{e410}';
    pub const APP: char = '\u{e426}';
}
