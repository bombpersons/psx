use psx_core::Machine;

use crate::{Angle, Exact, Fixed, Float, Mode, Num, Vec3};

/// `code` run on psx-core's R3000 with a0 = `a`, a1 = `b`: v0 after.
fn r3000(code: &[u32], a: u32, b: u32) -> u32 {
    let mut m = Machine::new(vec![0; 0x8_0000]);
    for (i, w) in code.iter().chain(&[0, 0]).enumerate() {
        let o = 0x1_0000 + 4 * i;
        m.bus.ram[o..o + 4].copy_from_slice(&w.to_le_bytes());
    }
    m.cpu.regs[4] = a;
    m.cpu.regs[5] = b;
    m.cpu.set_pc(0x8001_0000);
    for _ in 0..code.len() + 2 {
        m.step();
    }
    m.cpu.regs[2]
}

const MULT: u32 = 0x0085_0018; // mult a0, a1
const MULTU: u32 = 0x0085_0019; // multu a0, a1
const DIV: u32 = 0x0085_001A; // div a0, a1
const DIVU: u32 = 0x0085_001B; // divu a0, a1
const MFLO: u32 = 0x0000_1012; // mflo v0
const MFHI: u32 = 0x0000_1810; // mfhi v1
const OR: u32 = 0x0043_1025; // or v0, v0, v1
const fn sra(s: u32) -> u32 {
    0x0002_1003 | s << 6 // sra v0, v0, s
}
const fn srl(s: u32) -> u32 {
    0x0002_1002 | s << 6 // srl v0, v0, s
}
const fn sll_a0(s: u32) -> u32 {
    0x0004_2000 | s << 6 // sll a0, a0, s
}
const fn sll_v1(s: u32) -> u32 {
    0x0003_1800 | s << 6 // sll v1, v1, s
}

/// Values worth checking, and a spread of others.
fn values() -> Vec<u32> {
    let mut v = vec![0, 1, 0xFFFF_FFFF, 0x1000, 0xFFFF_F000, 0x7FFF_FFFF, 0x8000_0000, 0x1_0000, 0x7FFF, 0xFFFF_8000];
    let mut x = 0x2545_F491u32;
    for _ in 0..40 {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        v.push(x);
        v.push(x >> 12);
        v.push((x as i32 >> 16) as u32);
    }
    v
}

#[test]
fn products_and_quotients_match_the_r3000() {
    let vs = values();
    for &a in &vs {
        for &b in &vs {
            let (x, y) = (Fixed::<i32, 12>::from_bits(a as i32), Fixed::<i32, 12>::from_bits(b as i32));
            assert_eq!((x * y).bits() as u32, r3000(&[MULT, MFLO, sra(12)], a, b), "{a:#x} * {b:#x}");
            let wide = r3000(&[MULT, MFHI, MFLO, srl(12), sll_v1(20), OR], a, b);
            assert_eq!(x.mul_wide(y).bits() as u32, wide, "{a:#x} * {b:#x}, wide");
            assert_eq!((x / y).bits() as u32, r3000(&[sll_a0(12), DIV, MFLO], a, b), "{a:#x} / {b:#x}");
            let (u, w) = (Fixed::<u32, 8>::from_bits(a), Fixed::<u32, 8>::from_bits(b));
            assert_eq!((u * w).bits(), r3000(&[MULTU, MFLO, srl(8)], a, b), "{a:#x} * {b:#x} unsigned");
            assert_eq!((u / w).bits(), r3000(&[sll_a0(8), DIVU, MFLO], a, b), "{a:#x} / {b:#x} unsigned");
        }
    }
}

#[test]
fn narrow_words_compute_in_int_and_store_narrow() {
    // (short)((a * b) >> 12): the product is an int's.
    let a = Fixed::<i16, 12>::from_bits(0x7000);
    let b = Fixed::<i16, 12>::from_bits(0x2000);
    assert_eq!((a * b).bits(), ((0x7000i32 * 0x2000) >> 12) as i16);
    // Adding wraps at the store's width.
    assert_eq!((a + a).bits(), 0x7000i16.wrapping_add(0x7000));
    // A 16.16 times a 4.12 keeps the 16.16.
    let s = Fixed::<i32, 16>::real(2.5);
    assert_eq!((s * Fixed::<i16, 12>::real(0.5)).to_f64(), 1.25);
}

#[test]
fn named_operations() {
    let x = Fixed::<i32, 12>::from_bits(-0x1801); // -1.500244...
    assert_eq!(x.floor_int(), -2);
    assert_eq!(x.trunc_int(), -1);
    assert_eq!(x.snap_to_unit().bits(), -0x2000);
    assert_eq!(x.div_pow2_floor(4).bits(), -0x181);
    assert_eq!(x.div_pow2_trunc(4).bits(), -0x180);
    assert_eq!(x.rescale::<i16, 8>().bits(), -0x181);
    assert_eq!(x.rescale::<i32, 16>().bits(), -0x18010);
    assert_eq!(Fixed::<i32, 12>::from_bits(i32::MIN).abs().bits(), i32::MIN);
    // f32 settles where the original's truncation would.
    let f = Num::<Float, i32, 12>::real(0.0002);
    assert_eq!(f.settle_to_zero().to_f64(), 0.0);
    assert_eq!(Num::<Float, i32, 12>::real(0.01).settle_to_zero().to_f64(), 0.01f32 as f64);
    assert_eq!(Num::<Float, i32, 12>::real(-1.25).floor_int(), -2);
}

#[test]
fn reals_load_exactly_or_not_at_all() {
    assert_eq!(Fixed::<i32, 12>::real(1.375).bits(), 0x1600);
    assert_eq!(Fixed::<i32, 12>::try_real(0.1).unwrap_err(), "0.1 is not a multiple of 1/4096");
    assert_eq!(Fixed::<i16, 12>::try_real(8.0).unwrap_err(), "8 is outside -8 to 7.999755859375");
    let v: Vec3<Fixed<i32, 12>> = serde_json::from_str(r#"{"x": 1.375, "y": -2, "z": 0.000244140625}"#).unwrap();
    assert_eq!(v.map(|c| c.bits()), Vec3::new(0x1600, -0x2000, 1));
    assert_eq!(serde_json::to_string(&v).unwrap(), r#"{"x":1.375,"y":-2.0,"z":0.000244140625}"#);
    let e = serde_json::from_str::<Fixed<i32, 12>>("0.3").unwrap_err();
    assert!(e.to_string().contains("not a multiple of 1/4096"), "{e}");
    assert_eq!(serde_json::from_str::<Num<Float, i32, 12>>("0.3").unwrap().to_f32(), 0.3);
    let a: Angle<Exact, i16, 12> = serde_json::from_str("90").unwrap();
    assert_eq!(a.turns().bits(), 1024);
    assert_eq!(serde_json::to_string(&a).unwrap(), "90.0");
    assert!(serde_json::from_str::<Angle<Exact, i16, 12>>("0.1").is_err());
}

#[test]
fn angles_wrap_and_look_up_libgte_sine() {
    let table = &psx_gte::math::SIN_QUARTER;
    for units in -5000..5000 {
        let a = Angle::<Exact, i32, 12>::from_units(units);
        assert_eq!(a.sin_quarter::<12>(table).bits(), psx_gte::math::rsin(units as i32), "sin {units}");
        assert_eq!(a.cos_quarter::<12>(table).bits(), psx_gte::math::rcos(units as i32), "cos {units}");
    }
    let a = Angle::<Exact, i16, 12>::from_units(4000) + Angle::from_units(200);
    assert_eq!(a.turns().bits(), 4200);
    assert_eq!(a.normalized().turns().bits(), 104);
    assert_eq!(Angle::<Exact, i16, 12>::from_units(3000).normalized_signed().turns().bits(), -1096);
    let f = Angle::<Float, i16, 12>::degrees(450.0);
    assert_eq!(f.normalized().to_degrees(), 90.0);
    assert!((f.sin_quarter::<12>(table).to_f64() - 1.0).abs() < 1e-6);
    assert_eq!(Angle::<Float, i16, 12>::degrees(270.0).normalized_signed().to_degrees(), -90.0);
}

/// The crate's example, as a rule both modes run.
fn walk<M: Mode>(pos: &mut Num<M, i32, 12>, speed: Num<M, i32, 16>, heading: Angle<M, i16, 12>) {
    let step: Num<M, i32, 12> = (speed * heading.cos_quarter::<12>(&psx_gte::math::SIN_QUARTER)).rescale();
    *pos += step;
}

#[test]
fn one_rule_runs_in_both_modes() {
    let (mut e, mut f) = (Num::<Exact, i32, 12>::zero(), Num::<Float, i32, 12>::zero());
    for _ in 0..100 {
        walk(&mut e, Num::real(1.5), Angle::degrees(45.0));
        walk(&mut f, Num::real(1.5), Angle::degrees(45.0));
    }
    // cos 45° is 0xB50 / 4096 in libgte's table: the exact walk falls short.
    assert_eq!(e.bits(), 100 * ((0x18000i32 * 0xB50) >> 12 >> 4));
    assert!((f.to_f64() - 150.0 * 2f64.sqrt() / 2.0).abs() < 1e-3);
    assert!(e.to_f64() < f.to_f64());
}
