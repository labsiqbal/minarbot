//! Incompressible test images (large avatar uploads).

use crate::Result;
use image::{ImageFormat, Rgb, RgbImage};

/// PNG of pseudo-random pixels, which does not compress below
/// `width * height * 3` bytes by much.
pub fn noise_png(width: u32, height: u32, seed: u64) -> Result<Vec<u8>> {
    let mut state = seed | 1;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let image = RgbImage::from_fn(width, height, |_, _| {
        let value = next().to_le_bytes();
        Rgb([value[0], value[1], value[2]])
    });
    let mut bytes = std::io::Cursor::new(Vec::new());
    image.write_to(&mut bytes, ImageFormat::Png)?;
    Ok(bytes.into_inner())
}
