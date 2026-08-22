// src-tauri/src/servericon.rs
//
// `server-icon.png`: l'icona che Minecraft mostra nella lista multiplayer.
// Deve essere un PNG di esattamente 64×64: qualsiasi immagine viene
// ridimensionata (crop centrato per mantenere le proporzioni) e riscritta in PNG.

use base64::Engine;
use image::imageops::FilterType;
use image::ImageFormat;
use serde::Serialize;
use std::fs;
use std::io::Cursor;
use std::path::Path;

pub const FILE_NAME: &str = "server-icon.png";
pub const SIZE: u32 = 64;
pub const MAX_SOURCE_BYTES: usize = 25 * 1024 * 1024;

#[derive(Serialize, Clone, Debug)]
pub struct ServerIconInfo {
    pub data_url: String,
    pub size: u64,
    pub width: u32,
    pub height: u32,
}

/// Decodifica, ridimensiona a 64×64 (crop centrato) e codifica in PNG RGBA.
pub fn resize_to_server_icon(bytes: &[u8]) -> Result<Vec<u8>, String> {
    if bytes.is_empty() {
        return Err("Immagine vuota".to_string());
    }
    if bytes.len() > MAX_SOURCE_BYTES {
        return Err(format!("Immagine troppo grande ({} MB, max 25 MB)", bytes.len() / 1_048_576));
    }

    let img = image::load_from_memory(bytes)
        .map_err(|e| format!("Immagine non riconosciuta (usa PNG, JPG, WEBP, GIF o BMP): {}", e))?;

    let resized = if img.width() == SIZE && img.height() == SIZE {
        img
    } else {
        img.resize_to_fill(SIZE, SIZE, FilterType::Lanczos3)
    };

    let mut out = Cursor::new(Vec::new());
    resized
        .to_rgba8()
        .write_to(&mut out, ImageFormat::Png)
        .map_err(|e| format!("Codifica PNG fallita: {}", e))?;
    Ok(out.into_inner())
}

fn info_from_bytes(bytes: &[u8]) -> Result<ServerIconInfo, String> {
    let (width, height) = image::load_from_memory(bytes)
        .map(|i| (i.width(), i.height()))
        .unwrap_or((0, 0));
    Ok(ServerIconInfo {
        data_url: format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes)),
        size: bytes.len() as u64,
        width,
        height,
    })
}

/// Icona attuale del server, se presente.
pub fn read(server_dir: &Path) -> Result<Option<ServerIconInfo>, String> {
    let path = server_dir.join(FILE_NAME);
    if !path.is_file() {
        return Ok(None);
    }
    let bytes = fs::read(&path).map_err(|e| e.to_string())?;
    info_from_bytes(&bytes).map(Some)
}

/// Ridimensiona e scrive `server-icon.png`. Ritorna l'icona salvata.
pub fn write(server_dir: &Path, source: &[u8]) -> Result<ServerIconInfo, String> {
    let png = resize_to_server_icon(source)?;
    let path = server_dir.join(FILE_NAME);
    fs::write(&path, &png).map_err(|e| format!("Impossibile scrivere {}: {}", path.display(), e))?;
    info_from_bytes(&png)
}

pub fn write_from_path(server_dir: &Path, src: &Path) -> Result<ServerIconInfo, String> {
    if !src.is_file() {
        return Err("File non trovato".to_string());
    }
    let bytes = fs::read(src).map_err(|e| format!("Impossibile leggere {}: {}", src.display(), e))?;
    write(server_dir, &bytes)
}

pub fn remove(server_dir: &Path) -> Result<(), String> {
    let path = server_dir.join(FILE_NAME);
    if path.is_file() {
        fs::remove_file(&path).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgba};

    fn sample_png(w: u32, h: u32) -> Vec<u8> {
        let img = ImageBuffer::from_fn(w, h, |x, y| Rgba([(x % 256) as u8, (y % 256) as u8, 128u8, 255u8]));
        let mut out = Cursor::new(Vec::new());
        img.write_to(&mut out, ImageFormat::Png).unwrap();
        out.into_inner()
    }

    #[test]
    fn resizes_any_aspect_to_64x64_png() {
        for (w, h) in [(200, 100), (50, 300), (64, 64), (1024, 1024), (7, 9)] {
            let png = resize_to_server_icon(&sample_png(w, h)).unwrap();
            let decoded = image::load_from_memory(&png).unwrap();
            assert_eq!((decoded.width(), decoded.height()), (SIZE, SIZE), "sorgente {}x{}", w, h);
            assert_eq!(image::guess_format(&png).unwrap(), ImageFormat::Png);
        }
    }

    #[test]
    fn rejects_garbage_and_empty() {
        assert!(resize_to_server_icon(b"").is_err());
        assert!(resize_to_server_icon(b"not an image at all").is_err());
    }

    #[test]
    fn write_read_remove_roundtrip() {
        let d = std::env::temp_dir().join(format!("mineger-servericon-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();

        assert!(read(&d).unwrap().is_none());
        let info = write(&d, &sample_png(300, 120)).unwrap();
        assert_eq!((info.width, info.height), (64, 64));
        assert!(info.data_url.starts_with("data:image/png;base64,"));
        assert!(d.join(FILE_NAME).is_file());

        let again = read(&d).unwrap().unwrap();
        assert_eq!(again.size, info.size);

        remove(&d).unwrap();
        assert!(read(&d).unwrap().is_none());
        remove(&d).unwrap(); // idempotente
    }
}
