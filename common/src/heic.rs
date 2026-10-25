//! Photos HEIC/HEIF (iPhone…), décodées par ffmpeg : la bibliothèque `image`
//! ne les lit pas, et ffmpeg assemble les tuiles HEVC et applique la rotation
//! enregistrée dans le fichier. L'image arrive par un tuyau, en PNG non
//! compressé : aucun fichier intermédiaire.

use std::{path::Path, process::Command};

use image::DynamicImage;

/// Extension .heic ou .heif
pub fn is_heic(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "heic" | "heif"))
}

/// Décode la photo entière. Pas de réduction par ffmpeg : il assemble les
/// tuiles par un filtre complexe, qu'un `-vf scale` ne peut pas suivre.
pub fn decode(path: &Path) -> Result<DynamicImage, String> {
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-v", "error", "-nostdin", "-i"])
        .arg(path)
        .args(["-frames:v", "1"]);
    cmd.args([
        "-c:v",
        "png",
        "-compression_level",
        "0",
        "-f",
        "image2pipe",
        "-",
    ]);
    let out = cmd.output().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => "lecture des photos HEIC : ffmpeg introuvable".to_string(),
        _ => format!("ffmpeg : {e}"),
    })?;
    if !out.status.success() || out.stdout.is_empty() {
        let err = String::from_utf8_lossy(&out.stderr);
        let first = err.lines().next().unwrap_or("photo illisible");
        return Err(format!("HEIC : {first}"));
    }
    image::load_from_memory_with_format(&out.stdout, image::ImageFormat::Png)
        .map_err(|e| e.to_string())
}
