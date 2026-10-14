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
