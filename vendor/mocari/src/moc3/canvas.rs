use crate::{Error, Result};

use super::{Moc3Header, Moc3SectionOffsets, parse::read_f32};

const CANVAS_INFO_SIZE: usize = 64;
const F32_SIZE: usize = 4;

#[derive(Debug, Copy, Clone, PartialEq)]
/// Canvas information stored in a `.moc3` file.
///
/// The canvas describes the model coordinate system used by generated drawable
/// vertices.
pub struct Moc3CanvasInfo {
    pixels_per_unit: f32,
    origin_x: f32,
    origin_y: f32,
    width: f32,
    height: f32,
    flags: u8,
}

impl Moc3CanvasInfo {
    /// Parses canvas information from a full `.moc3` byte slice.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let header = Moc3Header::parse(bytes)?;
        let offsets = Moc3SectionOffsets::parse(bytes)?;
        let offset = usize::try_from(offsets.canvas_info_offset())
            .map_err(|_| invalid_canvas("canvas info offset does not fit in platform usize"))?;

        if bytes.len().saturating_sub(offset) < CANVAS_INFO_SIZE {
            return Err(invalid_canvas("canvas info is incomplete"));
        }

        Ok(Self {
            pixels_per_unit: read_f32(bytes, offset, header.endianness()),
            origin_x: read_f32(bytes, offset + F32_SIZE, header.endianness()),
            origin_y: read_f32(bytes, offset + F32_SIZE * 2, header.endianness()),
            width: read_f32(bytes, offset + F32_SIZE * 3, header.endianness()),
            height: read_f32(bytes, offset + F32_SIZE * 4, header.endianness()),
            flags: bytes[offset + F32_SIZE * 5],
        })
    }

    /// Returns the scale between model units and pixels.
    pub fn pixels_per_unit(&self) -> f32 {
        self.pixels_per_unit
    }

    /// Returns the x origin in model coordinates.
    pub fn origin_x(&self) -> f32 {
        self.origin_x
    }

    /// Returns the y origin in model coordinates.
    pub fn origin_y(&self) -> f32 {
        self.origin_y
    }

    /// Returns the canvas width in model units.
    pub fn width(&self) -> f32 {
        self.width
    }

    /// Returns the canvas height in model units.
    pub fn height(&self) -> f32 {
        self.height
    }

    /// Returns whether drawable y coordinates should be flipped for rendering.
    pub fn reverse_y_coordinate(&self) -> bool {
        self.flags & 1 == 1
    }
}

fn invalid_canvas(message: impl Into<String>) -> Error {
    Error::InvalidMoc3 {
        message: message.into(),
    }
}
