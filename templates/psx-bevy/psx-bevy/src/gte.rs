//! The GTE exactly as psx emulates it: its operations, flags and rounding
//! (`vendor/psx-gte`), and libgte's table sine for [`Angle`]s.
//!
//! Exact rules that ran on the GTE (a rotation by MVMVA, a projection by
//! RTPS) call these with the same inputs to get the original's results. The
//! numbers are [`psx_fixed`]'s ([`crate::num`]): [`to_vec3i16`] and
//! [`from_vec3i16`] move a vector between the two.

pub use psx_gte::*;

use psx_fixed::{Angle, Fixed, Mode, Num, Vec3, Word};

/// libgte's `rsin` and `rcos` for an angle of 4096 units to the turn: its
/// quarter-wave table in [`Exact`](psx_fixed::Exact), `f32::sin` and
/// `f32::cos` in [`Float`](psx_fixed::Float). Results are `1.3.12`.
pub trait GteAngle<M: Mode> {
    fn sin(self) -> Num<M, i32, 12>;
    fn cos(self) -> Num<M, i32, 12>;
}

impl<M: Mode, W: Word> GteAngle<M> for Angle<M, W, 12> {
    fn sin(self) -> Num<M, i32, 12> {
        self.sin_quarter(&math::SIN_QUARTER)
    }

    fn cos(self) -> Num<M, i32, 12> {
        self.cos_quarter(&math::SIN_QUARTER)
    }
}

/// A vector as the GTE's V0-V2 or a matrix row hold it.
pub fn to_vec3i16<const F: u32>(v: Vec3<Fixed<i16, F>>) -> Vec3i16 {
    Vec3i16::new(v.x.bits(), v.y.bits(), v.z.bits())
}

/// A GTE vector read as `F` fraction bits.
pub fn from_vec3i16<const F: u32>(v: Vec3i16) -> Vec3<Fixed<i16, F>> {
    Vec3::new(Fixed::from_bits(v.x), Fixed::from_bits(v.y), Fixed::from_bits(v.z))
}

/// A vector as the GTE's TR, MAC1-3 or a far colour hold it.
pub fn to_vec3i32<const F: u32>(v: Vec3<Fixed<i32, F>>) -> Vec3i32 {
    Vec3i32::new(v.x.bits(), v.y.bits(), v.z.bits())
}

/// A GTE vector read as `F` fraction bits.
pub fn from_vec3i32<const F: u32>(v: Vec3i32) -> Vec3<Fixed<i32, F>> {
    Vec3::new(Fixed::from_bits(v.x), Fixed::from_bits(v.y), Fixed::from_bits(v.z))
}

#[cfg(test)]
mod tests {
    use super::*;
    use psx_fixed::{Exact, Float};

    #[test]
    fn angles_use_libgte_sine() {
        for units in [0, 1, 341, 1024, 2047, 3000, 4095, 4096, -7] {
            let a = Angle::<Exact, i32, 12>::from_units(units);
            assert_eq!(a.sin().bits(), math::rsin(units as i32));
            assert_eq!(a.cos().bits(), math::rcos(units as i32));
        }
        assert!((Angle::<Float, i16, 12>::degrees(90.0).sin().to_f64() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn rotating_by_the_gte_matches_libgte() {
        let v = Vec3::new(Fixed::<i16, 0>::from_int(100), Fixed::from_int(0), Fixed::from_int(0));
        let m = math::rot_y(1024);
        let r = from_vec3i16::<0>(math::apply_matrix_sv(&m, to_vec3i16(v)));
        assert_eq!(r.map(|c| c.bits()), Vec3::new(0, 0, -100));
    }
}
