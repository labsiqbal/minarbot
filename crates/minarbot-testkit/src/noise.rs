//! Incompressible test images (large generated-image payloads).

use crate::Result;
use image::{ImageFormat, RgbImage};

/// PNG of `width` x `height` pseudo-random pixels from `seed`; noise keeps
/// the encoded size close to the raw `width * height * 3` bytes.
pub fn noise_png(width: u32, height: u32, seed: u64) -> Result<Vec<u8>> {
    let mut state = seed | 1;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 24) as u8
    };
    let image = RgbImage::from_fn(width, height, |_, _| image::Rgb([next(), next(), next()]));
    let mut bytes = std::io::Cursor::new(Vec::new());
    image.write_to(&mut bytes, ImageFormat::Png)?;
    Ok(bytes.into_inner())
}
