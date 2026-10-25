//! Une image ouverte : pixels d'origine (pour l'édition et l'enregistrement),
//! pyramide de réductions successives par 2 (pour un affichage rapide à toute
//! échelle), et informations (taille, date, EXIF).

use std::{
    path::{Path, PathBuf},
    time::SystemTime,
};

use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, RgbaImage};

/// Un niveau de la pyramide : pixels prémultipliés B, G, R, A
pub struct Level {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

pub struct Picture {
    pub path: PathBuf,
    pub source: RgbaImage,
    pub levels: Vec<Level>,
    pub format: Option<ImageFormat>,
    pub file_size: u64,
    pub modified: Option<SystemTime>,
    /// Lignes « libellé : valeur » des métadonnées de prise de vue
    pub exif: Vec<(String, String)>,
    /// Pivotée ou recadrée depuis l'ouverture (à enregistrer)
    pub edited: bool,
}

/// Extensions reconnues comme images
pub fn is_image(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        matches!(
            e.to_ascii_lowercase().as_str(),
            "jpg" | "jpeg" | "png" | "webp" | "gif" | "bmp" | "tif" | "tiff" | "heic" | "heif"
        )
    })
}

impl Picture {
    /// Décode l'image, en appliquant l'orientation EXIF (photo de téléphone).
    /// Une photo HEIC passe par ffmpeg (common::heic), déjà tournée.
    pub fn load(path: &Path) -> Result<Self, String> {
        let (img, format) = if common::heic::is_heic(path) {
            (common::heic::decode(path)?, None)
        } else {
            let reader = ImageReader::open(path)
                .map_err(|e| e.to_string())?
                .with_guessed_format()
                .map_err(|e| e.to_string())?;
            let format = reader.format();
            let mut decoder = reader.into_decoder().map_err(|e| e.to_string())?;
            let orientation = decoder.orientation().ok();
            let mut img = DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
            if let Some(o) = orientation {
                img.apply_orientation(o);
            }
            (img, format)
        };
        let meta = std::fs::metadata(path).ok();
        let mut picture = Self {
            path: path.to_path_buf(),
            source: img.to_rgba8(),
            levels: Vec::new(),
            format,
            file_size: meta.as_ref().map(|m| m.len()).unwrap_or(0),
            modified: meta.and_then(|m| m.modified().ok()),
            exif: read_exif(path),
            edited: false,
        };
        picture.build_levels();
        Ok(picture)
    }

    pub fn width(&self) -> u32 {
        self.source.width()
    }

    pub fn height(&self) -> u32 {
        self.source.height()
    }

    /// Niveau 0 (pleine résolution, prémultiplié) puis réductions par 2
    /// (moyenne de 2×2) jusqu'à moins de 256 pixels.
    fn build_levels(&mut self) {
        let (w, h) = self.source.dimensions();
        let mut data = Vec::with_capacity((w * h * 4) as usize);
        for px in self.source.pixels() {
            let [r, g, b, a] = px.0;
            let a32 = a as u32;
            data.extend_from_slice(&[
                (b as u32 * a32 / 255) as u8,
                (g as u32 * a32 / 255) as u8,
                (r as u32 * a32 / 255) as u8,
                a,
            ]);
        }
        let mut levels = vec![Level {
            width: w,
            height: h,
            data,
        }];
        loop {
            let last = levels.last().unwrap();
            if last.width.max(last.height) < 256 || last.width < 2 || last.height < 2 {
                break;
            }
            let (nw, nh) = (last.width / 2, last.height / 2);
            let mut next = vec![0u8; (nw * nh * 4) as usize];
            let stride = last.width as usize * 4;
            for y in 0..nh as usize {
                for x in 0..nw as usize {
                    let i = (2 * y) * stride + (2 * x) * 4;
                    let o = (y * nw as usize + x) * 4;
                    for c in 0..4 {
                        let sum = last.data[i + c] as u32
                            + last.data[i + 4 + c] as u32
                            + last.data[i + stride + c] as u32
                            + last.data[i + stride + 4 + c] as u32;
                        next[o + c] = ((sum + 2) / 4) as u8;
                    }
                }
            }
            levels.push(Level {
                width: nw,
                height: nh,
                data: next,
            });
        }
        self.levels = levels;
    }

    /// Rotation d'un quart de tour (sens horaire si `clockwise`).
    pub fn rotate(&mut self, clockwise: bool) {
        self.source = if clockwise {
            image::imageops::rotate90(&self.source)
        } else {
            image::imageops::rotate270(&self.source)
        };
        self.edited = true;
        self.build_levels();
    }

    /// Recadre au rectangle (x, y, w, h) en pixels de l'image.
    pub fn crop(&mut self, x: u32, y: u32, w: u32, h: u32) {
        let (iw, ih) = self.source.dimensions();
        let x = x.min(iw.saturating_sub(1));
        let y = y.min(ih.saturating_sub(1));
        let w = w.clamp(1, iw - x);
        let h = h.clamp(1, ih - y);
        self.source = image::imageops::crop_imm(&self.source, x, y, w, h).to_image();
        self.edited = true;
        self.build_levels();
    }

    /// Enregistre (à la place de l'original, ou sous `target`). Les métadonnées
    /// EXIF ne sont pas recopiées : l'orientation est déjà appliquée aux pixels.
    /// Une photo HEIC ne se réécrit pas : un JPEG est enregistré à côté.
    pub fn save(&mut self, target: Option<&Path>) -> Result<PathBuf, String> {
        let mut path = target.unwrap_or(&self.path).to_path_buf();
        if common::heic::is_heic(&path) {
            path = jpeg_beside(&path);
        }
        let format = ImageFormat::from_path(&path)
            .ok()
            .or(self.format)
            .unwrap_or(ImageFormat::Png);
        // Écriture dans un fichier voisin puis renommage : jamais d'original à moitié écrit
        let tmp = path.with_extension(format!(
            "{}.odysseus-tmp",
            path.extension().and_then(|e| e.to_str()).unwrap_or("img")
        ));
        let result = (|| -> Result<(), String> {
            let file = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
            let mut out = std::io::BufWriter::new(file);
            match format {
                ImageFormat::Jpeg => {
                    let rgb = DynamicImage::ImageRgba8(self.source.clone()).to_rgb8();
                    let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 92);
                    enc.encode_image(&rgb).map_err(|e| e.to_string())
                }
                _ => DynamicImage::ImageRgba8(self.source.clone())
                    .write_to(&mut out, format)
                    .map_err(|e| e.to_string()),
            }
        })();
        match result {
            Ok(()) => {
                std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
                if target.is_none() {
                    self.edited = false;
                }
                Ok(path)
            }
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                Err(e)
            }
        }
    }
}

/// « IMG_6113.HEIC » → « IMG_6113.jpg », ou une copie libre s'il existe déjà
fn jpeg_beside(path: &Path) -> PathBuf {
    let jpg = path.with_extension("jpg");
    if jpg.exists() { copy_name(&jpg) } else { jpg }
}

/// « photo.jpg » → « photo copie.jpg » (ou « photo copie 2.jpg »…), libre.
pub fn copy_name(path: &Path) -> PathBuf {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let dir = path.parent().unwrap_or(Path::new("."));
    let mut n = 1;
    loop {
        let name = if n == 1 {
            format!("{stem} copie{ext}")
        } else {
            format!("{stem} copie {n}{ext}")
        };
        let candidate = dir.join(name);
        if !candidate.exists() {
            return candidate;
        }
        n += 1;
    }
}

/// Métadonnées de prise de vue utiles, en français
fn read_exif(path: &Path) -> Vec<(String, String)> {
    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let mut reader = std::io::BufReader::new(file);
    let Ok(exif) = exif::Reader::new().read_from_container(&mut reader) else {
        return Vec::new();
    };
    let get = |tag| {
        exif.get_field(tag, exif::In::PRIMARY)
            .map(|f| {
                f.display_value()
                    .with_unit(&exif)
                    .to_string()
                    .trim_matches('"')
                    .trim()
                    .to_string()
            })
            .filter(|v| !v.is_empty())
    };
    let mut out = Vec::new();
    let camera = match (get(exif::Tag::Make), get(exif::Tag::Model)) {
        (Some(make), Some(model)) if model.starts_with(&make) => Some(model),
        (Some(make), Some(model)) => Some(format!("{make} {model}")),
        (a, b) => a.or(b),
    };
    if let Some(v) = camera {
        out.push(("Appareil".into(), v));
    }
    if let Some(v) = get(exif::Tag::LensModel) {
        out.push(("Objectif".into(), v));
    }
    if let Some(v) = get(exif::Tag::DateTimeOriginal) {
        out.push(("Prise de vue".into(), v));
    }
    if let Some(v) = get(exif::Tag::FocalLength) {
        out.push(("Focale".into(), v));
    }
    if let Some(v) = get(exif::Tag::FNumber) {
        out.push(("Ouverture".into(), v));
    }
    if let Some(v) = get(exif::Tag::ExposureTime) {
        out.push(("Exposition".into(), v));
    }
    if let Some(v) = get(exif::Tag::PhotographicSensitivity) {
        out.push(("ISO".into(), v));
    }
    out
}
