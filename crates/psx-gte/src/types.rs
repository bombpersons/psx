//! Small fixed-point value types used by the ergonomic layer.

/// Three i16 components. Used for vertices (integer units), normals and
/// angles (`1.3.12`, 4096 = 1.0 / 360°).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Vec3i16 {
    pub x: i16,
    pub y: i16,
    pub z: i16,
}

impl Vec3i16 {
    pub const ZERO: Self = Self::new(0, 0, 0);

    pub const fn new(x: i16, y: i16, z: i16) -> Self {
        Self { x, y, z }
    }

    pub const fn to_array(self) -> [i16; 3] {
        [self.x, self.y, self.z]
    }

    pub const fn from_array(a: [i16; 3]) -> Self {
        Self::new(a[0], a[1], a[2])
    }
}

/// Three i32 components. Used for translation (integer units), back/far
/// colors (`1.19.12`) and un-saturated MAC results.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Vec3i32 {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

impl Vec3i32 {
    pub const ZERO: Self = Self::new(0, 0, 0);

    pub const fn new(x: i32, y: i32, z: i32) -> Self {
        Self { x, y, z }
    }

    pub const fn to_array(self) -> [i32; 3] {
        [self.x, self.y, self.z]
    }

    pub const fn from_array(a: [i32; 3]) -> Self {
        Self::new(a[0], a[1], a[2])
    }
}

/// 3x3 matrix of `1.3.12` fixed-point elements (`0x1000` = 1.0), row-major:
/// `m[row][col]`. Matches libgte's `MATRIX.m` (the `t` translation part is
/// kept separately, see [`crate::Gte::set_translation`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Matrix {
    pub m: [[i16; 3]; 3],
}

impl Matrix {
    pub const ZERO: Self = Self { m: [[0; 3]; 3] };
    pub const IDENTITY: Self = Self {
        m: [[0x1000, 0, 0], [0, 0x1000, 0], [0, 0, 0x1000]],
    };

    pub const fn new(m: [[i16; 3]; 3]) -> Self {
        Self { m }
    }

    pub const fn transpose(&self) -> Self {
        let m = self.m;
        Self::new([
            [m[0][0], m[1][0], m[2][0]],
            [m[0][1], m[1][1], m[2][1]],
            [m[0][2], m[1][2], m[2][2]],
        ])
    }
}

/// An RGB color plus the GPU command/code byte, as packed in RGBC / RGB0-2
/// (`r` in bits 0-7, `g` 8-15, `b` 16-23, `code` 24-31).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Rgbc {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub code: u8,
}

impl Rgbc {
    pub const fn new(r: u8, g: u8, b: u8, code: u8) -> Self {
        Self { r, g, b, code }
    }

    pub const fn from_u32(v: u32) -> Self {
        Self::new(v as u8, (v >> 8) as u8, (v >> 16) as u8, (v >> 24) as u8)
    }

    pub const fn to_u32(self) -> u32 {
        self.r as u32 | (self.g as u32) << 8 | (self.b as u32) << 16 | (self.code as u32) << 24
    }
}

/// A screen coordinate as packed in SXY0-2 (`x` low half, `y` high half).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ScreenXY {
    pub x: i16,
    pub y: i16,
}

impl ScreenXY {
    pub const fn new(x: i16, y: i16) -> Self {
        Self { x, y }
    }

    pub const fn from_u32(v: u32) -> Self {
        Self::new(v as i16, (v >> 16) as i16)
    }

    pub const fn to_u32(self) -> u32 {
        self.x as u16 as u32 | (self.y as u16 as u32) << 16
    }
}
