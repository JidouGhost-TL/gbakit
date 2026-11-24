//! GBA Object Attribute Memory (OAM) sprite attributes and affine matrices.

use formatkit_core::{Error, Result};

/// GBA OAM sprite shape (Attr0 bits 14..=15).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjShape {
    Square = 0,
    Horizontal = 1,
    Vertical = 2,
}

/// GBA OAM sprite rendering mode (Attr0 bits 10..=11).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjMode {
    Normal = 0,
    SemiTransparent = 1,
    ObjWindow = 2,
}

/// GBA OAM sprite color depth (Attr0 bit 13).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjColorMode {
    /// 4 bits per pixel (16 colors per palette bank, 16 banks).
    Bpp4 = 0,
    /// 8 bits per pixel (256 colors, single palette).
    Bpp8 = 1,
}

/// Decoded GBA OAM sprite attributes (`attr0`, `attr1`, `attr2`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjAttr {
    /// Y coordinate (`0..=255`).
    pub y: u8,
    /// Rotation/scaling (affine) flag (Attr0 bit 8).
    pub affine: bool,
    /// Double-size flag when `affine` is `true`, or OBJ disable flag when `affine` is `false` (Attr0 bit 9).
    pub double_size_or_disable: bool,
    /// Rendering mode (Attr0 bits 10..=11).
    pub mode: ObjMode,
    /// Mosaic enable flag (Attr0 bit 12).
    pub mosaic: bool,
    /// Color depth (Attr0 bit 13).
    pub color_mode: ObjColorMode,
    /// Sprite shape (Attr0 bits 14..=15).
    pub shape: ObjShape,
    /// X coordinate (`0..=511`, Attr1 bits 0..=8).
    pub x: u16,
    /// Affine parameter index (`0..=31`, Attr1 bits 9..=13 when `affine` is `true`).
    pub affine_param: u8,
    /// Horizontal flip (Attr1 bit 12 when `affine` is `false`).
    pub h_flip: bool,
    /// Vertical flip (Attr1 bit 13 when `affine` is `false`).
    pub v_flip: bool,
    /// Sprite size code (`0..=3`, Attr1 bits 14..=15).
    pub size: u8,
    /// Base tile index (`0..=1023`, Attr2 bits 0..=9).
    pub tile_index: u16,
    /// Priority relative to backgrounds (`0..=3`, Attr2 bits 10..=11).
    pub priority: u8,
    /// Palette bank (`0..=15`, Attr2 bits 12..=15; used in 4bpp mode).
    pub palette_bank: u8,
}

impl ObjAttr {
    /// Decode an `ObjAttr` from three 16-bit OAM attribute words `[attr0, attr1, attr2]`.
    pub fn from_attrs(attrs: [u16; 3]) -> Result<Self> {
        let [attr0, attr1, attr2] = attrs;

        let y = (attr0 & 0x00FF) as u8;
        let affine = (attr0 & 0x0100) != 0;
        let double_size_or_disable = (attr0 & 0x0200) != 0;

        let mode = match (attr0 >> 10) & 0x03 {
            0 => ObjMode::Normal,
            1 => ObjMode::SemiTransparent,
            2 => ObjMode::ObjWindow,
            raw => {
                return Err(Error::InvalidField {
                    what: "obj_mode",
                    value: u64::from(raw),
                });
            }
        };

        let mosaic = (attr0 & 0x1000) != 0;
        let color_mode = if (attr0 & 0x2000) != 0 {
            ObjColorMode::Bpp8
        } else {
            ObjColorMode::Bpp4
        };

        let shape = match (attr0 >> 14) & 0x03 {
            0 => ObjShape::Square,
            1 => ObjShape::Horizontal,
            2 => ObjShape::Vertical,
            raw => {
                return Err(Error::InvalidField {
                    what: "obj_shape",
                    value: u64::from(raw),
                });
            }
        };

        let x = attr1 & 0x01FF;
        let (affine_param, h_flip, v_flip) = if affine {
            (((attr1 >> 9) & 0x1F) as u8, false, false)
        } else {
            (0, (attr1 & 0x1000) != 0, (attr1 & 0x2000) != 0)
        };
        let size = ((attr1 >> 14) & 0x03) as u8;

        let tile_index = attr2 & 0x03FF;
        let priority = ((attr2 >> 10) & 0x03) as u8;
        let palette_bank = ((attr2 >> 12) & 0x0F) as u8;

        Ok(Self {
            y,
            affine,
            double_size_or_disable,
            mode,
            mosaic,
            color_mode,
            shape,
            x,
            affine_param,
            h_flip,
            v_flip,
            size,
            tile_index,
            priority,
            palette_bank,
        })
    }

    /// Decode an `ObjAttr` from 6 little-endian bytes (`attr0`, `attr1`, `attr2`).
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 6 {
            return Err(Error::Truncated {
                offset: 0,
                needed: 6,
                available: bytes.len(),
            });
        }
        let attrs = [
            u16::from_le_bytes([bytes[0], bytes[1]]),
            u16::from_le_bytes([bytes[2], bytes[3]]),
            u16::from_le_bytes([bytes[4], bytes[5]]),
        ];
        Self::from_attrs(attrs)
    }

    /// Decode an `ObjAttr` from an 8-byte OAM entry, ignoring the trailing 2-byte affine padding.
    pub fn from_oam_entry(entry: &[u8; 8]) -> Result<Self> {
        Self::from_attrs([
            u16::from_le_bytes([entry[0], entry[1]]),
            u16::from_le_bytes([entry[2], entry[3]]),
            u16::from_le_bytes([entry[4], entry[5]]),
        ])
    }

    /// Encode this `ObjAttr` back into three 16-bit OAM attribute words `[attr0, attr1, attr2]`.
    pub fn to_attrs(self) -> [u16; 3] {
        let attr0 = u16::from(self.y)
            | if self.affine { 0x0100 } else { 0 }
            | if self.double_size_or_disable {
                0x0200
            } else {
                0
            }
            | ((self.mode as u16) << 10)
            | if self.mosaic { 0x1000 } else { 0 }
            | ((self.color_mode as u16) << 13)
            | ((self.shape as u16) << 14);

        let attr1_mid = if self.affine {
            u16::from(self.affine_param & 0x1F) << 9
        } else {
            (if self.h_flip { 0x1000 } else { 0 }) | (if self.v_flip { 0x2000 } else { 0 })
        };
        let attr1 = (self.x & 0x01FF) | attr1_mid | (u16::from(self.size & 0x03) << 14);

        let attr2 = (self.tile_index & 0x03FF)
            | (u16::from(self.priority & 0x03) << 10)
            | (u16::from(self.palette_bank & 0x0F) << 12);

        [attr0, attr1, attr2]
    }

    /// Return the exact GBA hardware `(width, height)` in pixels for all 12 valid `(shape, size)` pairs.
    pub fn dimensions(self) -> Option<(u16, u16)> {
        match (self.shape, self.size) {
            (ObjShape::Square, 0) => Some((8, 8)),
            (ObjShape::Square, 1) => Some((16, 16)),
            (ObjShape::Square, 2) => Some((32, 32)),
            (ObjShape::Square, 3) => Some((64, 64)),
            (ObjShape::Horizontal, 0) => Some((16, 8)),
            (ObjShape::Horizontal, 1) => Some((32, 8)),
            (ObjShape::Horizontal, 2) => Some((32, 16)),
            (ObjShape::Horizontal, 3) => Some((64, 32)),
            (ObjShape::Vertical, 0) => Some((8, 16)),
            (ObjShape::Vertical, 1) => Some((8, 32)),
            (ObjShape::Vertical, 2) => Some((16, 32)),
            (ObjShape::Vertical, 3) => Some((32, 64)),
            _ => None,
        }
    }

    /// Returns `true` if this non-affine sprite is disabled (not displayed).
    pub fn is_disabled(self) -> bool {
        !self.affine && self.double_size_or_disable
    }

    /// Returns `true` if this affine sprite uses double-size rendering bounds.
    pub fn is_double_size(self) -> bool {
        self.affine && self.double_size_or_disable
    }
}

/// GBA OAM affine transformation matrix (`pa`, `pb`, `pc`, `pd`) in 8.8 signed fixed-point format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjAffine {
    pub pa: i16,
    pub pb: i16,
    pub pc: i16,
    pub pd: i16,
}

impl ObjAffine {
    /// Extract an affine matrix from a 32-byte (4-entry) OAM block.
    ///
    /// GBA OAM interleaves the four 16-bit signed parameters at byte offsets
    /// `6..8` (`pa`), `14..16` (`pb`), `22..24` (`pc`), and `30..32` (`pd`).
    pub fn from_oam_block(block: &[u8; 32]) -> Self {
        Self {
            pa: i16::from_le_bytes([block[6], block[7]]),
            pb: i16::from_le_bytes([block[14], block[15]]),
            pc: i16::from_le_bytes([block[22], block[23]]),
            pd: i16::from_le_bytes([block[30], block[31]]),
        }
    }

    /// Convert the 8.8 signed fixed-point entries to `[pa, pb, pc, pd]` floating-point values.
    pub fn to_f32(self) -> [f32; 4] {
        [
            f32::from(self.pa) / 256.0,
            f32::from(self.pb) / 256.0,
            f32::from(self.pc) / 256.0,
            f32::from(self.pd) / 256.0,
        ]
    }
}
