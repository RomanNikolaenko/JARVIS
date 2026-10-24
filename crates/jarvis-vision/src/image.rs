//! Preparing images for vision models: decoding with limits, downscaling
//! and JPEG encoding, plus a small preview for the UI.

use std::io::Cursor;

use base64::Engine as _;
use image::imageops::FilterType;
use image::{DynamicImage, ImageFormat, ImageReader, Limits, RgbImage, RgbaImage};

use crate::error::{Result, VisionError};

/// Longest side sent to models. Larger images cost tokens without helping
/// most models, which downscale them anyway.
pub const MAX_LONG_SIDE: u32 = 1568;
pub const PREVIEW_LONG_SIDE: u32 = 320;
/// Largest image file accepted.
pub const MAX_FILE_BYTES: usize = 25 * 1024 * 1024;
const MAX_DIMENSION: u32 = 16_384;
const MAX_DECODE_BYTES: u64 = 512 * 1024 * 1024;
const JPEG_QUALITY: u8 = 85;

/// An image as sent to a model.
#[derive(Clone, PartialEq, Eq)]
pub struct PreparedImage {
    pub width: u32,
    pub height: u32,
    pub jpeg: Vec<u8>,
    pub preview_jpeg: Vec<u8>,
}

impl std::fmt::Debug for PreparedImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedImage")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("jpeg_len", &self.jpeg.len())
            .finish_non_exhaustive()
    }
}

impl PreparedImage {
    pub fn base64(&self) -> String {
        base64::engine::general_purpose::STANDARD.encode(&self.jpeg)
    }

    pub fn preview_data_url(&self) -> String {
        format!(
            "data:image/jpeg;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&self.preview_jpeg)
        )
    }
}

/// Decodes a PNG or JPEG file, refusing oversized or malformed input.
pub fn decode(bytes: &[u8]) -> Result<RgbaImage> {
    if bytes.len() > MAX_FILE_BYTES {
        return Err(VisionError::InvalidImage(format!(
            "the image is larger than {} MB",
            MAX_FILE_BYTES / 1024 / 1024
        )));
    }
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|err| VisionError::InvalidImage(err.to_string()))?;
    if !matches!(reader.format(), Some(ImageFormat::Png | ImageFormat::Jpeg)) {
        return Err(VisionError::InvalidImage(
            "only PNG and JPEG images are supported".into(),
        ));
    }
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_DIMENSION);
    limits.max_image_height = Some(MAX_DIMENSION);
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    reader.limits(limits);
    let image = reader
        .decode()
        .map_err(|err| VisionError::InvalidImage(format!("cannot read the image: {err}")))?;
    Ok(image.into_rgba8())
}

/// Decodes base64 image data (photos from the camera).
pub fn decode_base64(data: &str) -> Result<RgbaImage> {
    let data = data
        .split_once(";base64,")
        .map_or(data, |(_, payload)| payload);
    if data.len() > MAX_FILE_BYTES / 3 * 4 + 4 {
        return Err(VisionError::InvalidImage("the image is too large".into()));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data.trim())
        .map_err(|_| VisionError::InvalidImage("the image data is not valid base64".into()))?;
    decode(&bytes)
}

/// Downscales to at most [`MAX_LONG_SIDE`] and encodes JPEG, with a preview.
pub fn prepare(image: RgbaImage) -> Result<PreparedImage> {
    if image.width() == 0 || image.height() == 0 {
        return Err(VisionError::InvalidImage("the image is empty".into()));
    }
    let rgb = flatten(image);
    let full = fit(rgb, MAX_LONG_SIDE, FilterType::Triangle);
    let preview = fit(full.clone(), PREVIEW_LONG_SIDE, FilterType::Triangle);
    Ok(PreparedImage {
        width: full.width(),
        height: full.height(),
        jpeg: encode_jpeg(&full)?,
        preview_jpeg: encode_jpeg(&preview)?,
    })
}

/// Transparent pixels over white (JPEG has no alpha).
fn flatten(image: RgbaImage) -> RgbImage {
    let (width, height) = image.dimensions();
    let mut rgb = RgbImage::new(width, height);
    for (out, pixel) in rgb.pixels_mut().zip(image.pixels()) {
        let alpha = u16::from(pixel[3]);
        for channel in 0..3 {
            let value = u16::from(pixel[channel]) * alpha + 255 * (255 - alpha);
            out[channel] = u8::try_from(value / 255).unwrap_or(u8::MAX);
        }
    }
    rgb
}

fn fit(image: RgbImage, long_side: u32, filter: FilterType) -> RgbImage {
    let (width, height) = image.dimensions();
    let longest = width.max(height);
    if longest <= long_side {
        return image;
    }
    let scale = f64::from(long_side) / f64::from(longest);
    let new_width = ((f64::from(width) * scale).round() as u32).max(1);
    let new_height = ((f64::from(height) * scale).round() as u32).max(1);
    image::imageops::resize(&image, new_width, new_height, filter)
}

fn encode_jpeg(image: &RgbImage) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY);
    DynamicImage::ImageRgb8(image.clone())
        .write_with_encoder(encoder)
        .map_err(|err| VisionError::InvalidImage(format!("cannot encode the image: {err}")))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let image = RgbaImage::from_fn(width, height, |x, y| {
            image::Rgba([(x % 256) as u8, (y % 256) as u8, 128, 255])
        });
        let mut out = Vec::new();
        DynamicImage::ImageRgba8(image)
            .write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
            .expect("encode");
        out
    }

    #[test]
    fn large_screenshots_are_downscaled_and_previewed() {
        let image = decode(&png(3840, 2160)).expect("decode");
        let prepared = prepare(image).expect("prepare");
        assert_eq!((prepared.width, prepared.height), (1568, 882));
        assert_eq!(&prepared.jpeg[..2], &[0xFF, 0xD8], "JPEG");
        let preview = decode(&prepared.preview_jpeg).expect("preview");
        assert_eq!(preview.width(), PREVIEW_LONG_SIDE);
        assert!(
            prepared
                .preview_data_url()
                .starts_with("data:image/jpeg;base64,/9j/")
        );
        // Small images keep their size.
        let small = prepare(decode(&png(200, 100)).expect("decode")).expect("prepare");
        assert_eq!((small.width, small.height), (200, 100));
    }

    #[test]
    fn camera_photos_arrive_as_base64() {
        let jpeg = prepare(decode(&png(64, 48)).expect("decode"))
            .expect("prepare")
            .jpeg;
        let data = format!(
            "data:image/jpeg;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&jpeg)
        );
        let photo = decode_base64(&data).expect("photo");
        assert_eq!(photo.dimensions(), (64, 48));
        assert!(decode_base64("%%%").is_err());
    }

    #[test]
    fn other_formats_and_garbage_are_refused() {
        assert!(decode(b"not an image").is_err());
        assert!(decode(b"GIF89a....").is_err());
        let mut truncated = png(100, 100);
        truncated.truncate(60);
        assert!(decode(&truncated).is_err());
        assert!(prepare(RgbaImage::new(0, 0)).is_err());
    }

    #[test]
    fn transparency_becomes_white() {
        let image = RgbaImage::from_pixel(2, 2, image::Rgba([0, 0, 0, 0]));
        let rgb = flatten(image);
        assert_eq!(rgb.get_pixel(0, 0).0, [255, 255, 255]);
    }
}
