//! Vectors of numbers, component by component.

use std::ops::{Add, AddAssign, Div, Mul, Neg, Sub, SubAssign};

use crate::mode::Mode;
use crate::num::Num;
use crate::word::Word;

/// Two components. Arithmetic is component by component; anything that
/// mixes them (a dot product, a length) rounds the way the original does,
/// so the port writes it out from [`Num`] operations.
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Vec2<T> {
    pub x: T,
    pub y: T,
}

/// Three components, like [`Vec2`].
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Vec3<T> {
    pub x: T,
    pub y: T,
    pub z: T,
}

impl<T> Vec2<T> {
    pub const fn new(x: T, y: T) -> Self {
        Vec2 { x, y }
    }

    pub fn map<U>(self, mut f: impl FnMut(T) -> U) -> Vec2<U> {
        Vec2 { x: f(self.x), y: f(self.y) }
    }
}

impl<T> Vec3<T> {
    pub const fn new(x: T, y: T, z: T) -> Self {
        Vec3 { x, y, z }
    }

    pub fn map<U>(self, mut f: impl FnMut(T) -> U) -> Vec3<U> {
        Vec3 { x: f(self.x), y: f(self.y), z: f(self.z) }
    }
}

impl<M: Mode, W: Word, const F: u32> Vec2<Num<M, W, F>> {
    /// In another format, each component as [`Num::rescale`].
    pub fn rescale<W2: Word, const G: u32>(self) -> Vec2<Num<M, W2, G>> {
        self.map(Num::rescale)
    }

    /// The real values as `f32`, for rendering.
    pub fn to_f32(self) -> [f32; 2] {
        [self.x.to_f32(), self.y.to_f32()]
    }
}

impl<M: Mode, W: Word, const F: u32> Vec3<Num<M, W, F>> {
    /// In another format, each component as [`Num::rescale`].
    pub fn rescale<W2: Word, const G: u32>(self) -> Vec3<Num<M, W2, G>> {
        self.map(Num::rescale)
    }

    /// The real values as `f32`, for rendering.
    pub fn to_f32(self) -> [f32; 3] {
        [self.x.to_f32(), self.y.to_f32(), self.z.to_f32()]
    }
}

macro_rules! ops {
    ($v:ident { $($c:ident),* }) => {
        impl<T: Add<Output = T>> Add for $v<T> {
            type Output = Self;
            fn add(self, b: Self) -> Self {
                $v { $($c: self.$c + b.$c),* }
            }
        }

        impl<T: Sub<Output = T>> Sub for $v<T> {
            type Output = Self;
            fn sub(self, b: Self) -> Self {
                $v { $($c: self.$c - b.$c),* }
            }
        }

        impl<T: Neg<Output = T>> Neg for $v<T> {
            type Output = Self;
            fn neg(self) -> Self {
                $v { $($c: -self.$c),* }
            }
        }

        impl<T: Copy + Add<Output = T>> AddAssign for $v<T> {
            fn add_assign(&mut self, b: Self) {
                *self = *self + b;
            }
        }

        impl<T: Copy + Sub<Output = T>> SubAssign for $v<T> {
            fn sub_assign(&mut self, b: Self) {
                *self = *self - b;
            }
        }

        /// Each component times `s`.
        impl<T: Mul<S, Output = T>, S: Copy> Mul<S> for $v<T> {
            type Output = Self;
            fn mul(self, s: S) -> Self {
                $v { $($c: self.$c * s),* }
            }
        }

        /// Each component divided by `s`.
        impl<T: Div<S, Output = T>, S: Copy> Div<S> for $v<T> {
            type Output = Self;
            fn div(self, s: S) -> Self {
                $v { $($c: self.$c / s),* }
            }
        }
    };
}

ops!(Vec2 { x, y });
ops!(Vec3 { x, y, z });
