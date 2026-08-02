use std::fs;
use std::path::Path;

use fontdue::{Font, FontSettings};
use thiserror::Error;

pub mod footer;
pub mod radar;
pub mod setup;
pub mod text;
pub mod theme;

const EMBEDDED_FONT: &[u8] = include_bytes!("../assets/DejaVuSans-Bold.ttf");

#[derive(Clone, Debug)]
pub struct FontAsset {
    font: Font,
}

impl FontAsset {
    pub fn from_static(bytes: &'static [u8]) -> Result<Self, RenderError> {
        Font::from_bytes(
            bytes,
            FontSettings {
                collection_index: 0,
                scale: 40.0,
                load_substitutions: true,
            },
        )
        .map(|font| Self { font })
        .map_err(|error| RenderError::InvalidFont(error.to_owned()))
    }

    pub fn embedded() -> Result<Self, RenderError> {
        Self::from_static(EMBEDDED_FONT)
    }

    pub(crate) fn font(&self) -> &Font {
        &self.font
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

impl Frame {
    pub fn new(width: u32, height: u32, rgba: Vec<u8>) -> Result<Self, RenderError> {
        if width == 0 || height == 0 {
            return Err(RenderError::InvalidDimensions { width, height });
        }
        let expected = frame_length(width, height)?;
        if rgba.len() != expected {
            return Err(RenderError::InvalidFrameLength {
                expected,
                actual: rgba.len(),
            });
        }
        Ok(Self {
            width,
            height,
            rgba,
        })
    }

    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn pixels(&self) -> &[u8] {
        &self.rgba
    }

    pub fn save_png(&self, path: &Path) -> Result<(), RenderError> {
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let temporary = tempfile::NamedTempFile::new_in(parent)?;
        {
            let mut encoder = png::Encoder::new(temporary.as_file(), self.width, self.height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header()?;
            writer.write_image_data(&self.rgba)?;
            writer.finish()?;
        }
        temporary.as_file().sync_all()?;
        temporary
            .persist(path)
            .map_err(|error| RenderError::Io(error.error))?;
        sync_parent(parent)?;
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum RenderError {
    #[error("font data is invalid: {0}")]
    InvalidFont(String),
    #[error("frame dimensions must be nonzero, got {width}x{height}")]
    InvalidDimensions { width: u32, height: u32 },
    #[error("frame dimensions overflow the address space")]
    DimensionsOverflow,
    #[error("frame has {actual} bytes, expected {expected}")]
    InvalidFrameLength { expected: usize, actual: usize },
    #[error("PNG encoding failed: {0}")]
    Png(#[from] png::EncodingError),
    #[error("QR encoding failed: {0}")]
    Qr(#[from] qrcode::types::QrError),
    #[error("render I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("radar location is not configured")]
    UnconfiguredLocation,
    #[error("invalid radar settings: {0}")]
    InvalidSettings(&'static str),
    #[error("radar geometry failed: {0}")]
    Geometry(#[from] crate::geometry::GeometryError),
    #[error("radar range failed: {0}")]
    Range(#[from] crate::range::RangeError),
}

fn frame_length(width: u32, height: u32) -> Result<usize, RenderError> {
    usize::try_from(width)
        .ok()
        .and_then(|width| {
            usize::try_from(height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or(RenderError::DimensionsOverflow)
}

fn sync_parent(parent: &Path) -> Result<(), RenderError> {
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}
