//! Small image helpers for avatar and attachment tests.

use crate::Result;
use image::{ImageFormat, Rgba, RgbaImage};

/// Solid-colour PNG of `width` x `height`.
pub fn png(width: u32, height: u32, color: [u8; 4]) -> Result<Vec<u8>> {
    let image = RgbaImage::from_pixel(width, height, Rgba(color));
    let mut bytes = std::io::Cursor::new(Vec::new());
    image.write_to(&mut bytes, ImageFormat::Png)?;
    Ok(bytes.into_inner())
}

/// Width and height of encoded image bytes.
pub fn dimensions(bytes: &[u8]) -> Result<(u32, u32)> {
    let image = image::load_from_memory(bytes)?;
    Ok((image.width(), image.height()))
}
