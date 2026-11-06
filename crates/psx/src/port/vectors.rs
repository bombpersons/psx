//! `psx port vectors`: test vectors for ports in any language, made by the
//! crates Rust ports use (psx-fixed's `Exact` mode and psx-gte) as the
//! reference. The rules they test are the guide's topic `languages`.
//!
//! Each file is JSON lines, one case a line:
//!
//! - `fixed.jsonl`: `{"op", "a", "b"?, "n"?, "k"?, "to"?, "out"}`, where a
//!   number is `[word, fraction bits, raw]` and `out` the result's raw word
//!   (an `int` for `floor_int` and `trunc_int`, the table's value for the
//!   angle helpers).
//! - `gte.jsonl`: `{"op":"execute", "command", "data", "ctrl", "out"}` (the
//!   registers written, null where not, then the 64 read after the command)
//!   and the libgte-style helpers by name.
//! - `sin_quarter.json` and `unr_table.json`: the tables.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use psx_fixed::{Angle, Exact, Fixed, Word};
use psx_gte::{Gte, Matrix, Vec3i16, math};
use serde_json::{Value, json};

use crate::Error;

#[derive(clap::Args, Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct PortVectorsArgs {
    /// The directory to write the vectors in; default `.psx/port/vectors/`.
    #[arg(long)]
    #[serde(default)]
    pub out: Option<PathBuf>,
}

impl crate::commands::Op for PortVectorsArgs {
    fn exec(self, ctx: &crate::commands::Ctx) -> crate::commands::Outcome {
        let dir = self.out.map(|d| ctx.root.join(d)).unwrap_or_else(|| ctx.root.join(".psx/port/vectors"));
        crate::commands::Outcome::result(write(&dir))
    }
}

/// The cases per format and operation drawn at random, after the edges.
const RANDOM: usize = 48;
/// The cases per GTE command and `sf`/`lm`.
const GTE_CASES: usize = 24;

fn write(dir: &Path) -> crate::Result<Value> {
    std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
    let mut files = Vec::new();
    for (name, lines) in [("fixed.jsonl", fixed()), ("gte.jsonl", gte())] {
        let mut text = String::new();
        for l in &lines {
            let _ = writeln!(text, "{l}");
        }
        let p = dir.join(name);
        crate::write_atomic(&p, text.as_bytes()).map_err(|e| Error::io(&p, e))?;
        files.push(json!({"path": p, "cases": lines.len()}));
    }
    for (name, table) in [
        ("sin_quarter.json", json!(math::SIN_QUARTER.to_vec())),
        ("unr_table.json", json!(psx_gte::UNR_TABLE.to_vec())),
    ] {
        let p = dir.join(name);
        crate::write_atomic(&p, table.to_string().as_bytes()).map_err(|e| Error::io(&p, e))?;
        files.push(json!({"path": p}));
    }
    Ok(json!({
        "dir": dir,
        "files": files,
        "rules": "psx guide --topic languages",
        "next": ["run them in the port's own tests: each case's out is what the original computes"],
    }))
}

/// splitmix64: the same cases on every machine.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Mostly small magnitudes, where games keep their numbers, sometimes
    /// any bits.
    fn raw<W: Word>(&mut self) -> i64 {
        let bits = match self.next() % 4 {
            0 => W::BITS,
            _ => 1 + (self.next() % W::BITS as u64) as u32,
        };
        W::wrap((self.next() >> (64 - bits)) as i64 * if self.next().is_multiple_of(2) { 1 } else { -1 }).get()
    }
}

fn word<W: Word>() -> String {
    format!("{}{}", if W::SIGNED { "i" } else { "u" }, W::BITS)
}

fn num<W: Word, const F: u32>(raw: i64) -> Value {
    json!([word::<W>(), F, raw])
}

fn get<W: Word, const F: u32>(x: Fixed<W, F>) -> i64 {
    x.bits().get()
}

fn fx<W: Word, const F: u32>(raw: i64) -> Fixed<W, F> {
    Fixed::from_raw(raw)
}

/// The edges of a format: its limits, zero, ±1 and around one unit.
fn edges<W: Word, const F: u32>() -> Vec<i64> {
    let one = 1i64 << F;
    let mut v = vec![W::MIN, W::MIN + 1, -one - 1, -one, -1, 0, 1, one - 1, one, one + 1, W::MAX - 1, W::MAX];
    v.retain(|x| (W::MIN..=W::MAX).contains(x));
    v.dedup();
    v
}

fn cases<W: Word, const F: u32>(rng: &mut Rng) -> Vec<i64> {
    let mut v = edges::<W, F>();
    v.extend((0..RANDOM).map(|_| rng.raw::<W>()));
    v
}

/// The operations on one format.
fn unary<W: Word, const F: u32>(rng: &mut Rng, out: &mut Vec<Value>) {
    let xs = cases::<W, F>(rng);
    let a = |x: i64| num::<W, F>(x);
    for (i, &x) in xs.iter().enumerate() {
        let y = xs[(i * 7 + 3) % xs.len()];
        let n = [0, 1, -1, 2, 3, -7, 100, i32::MIN, i32::MAX][i % 9];
        let n = if i >= 9 { (rng.next() as i32) >> (rng.next() % 31) } else { n };
        let k = [0, 1, 4, 12, 31, 32, 33, 63][i % 8];
        let (p, q) = (fx::<W, F>(x), fx::<W, F>(y));
        out.push(json!({"op": "add", "a": a(x), "b": a(y), "out": get(p + q)}));
        out.push(json!({"op": "sub", "a": a(x), "b": a(y), "out": get(p - q)}));
        out.push(json!({"op": "neg", "a": a(x), "out": get(-p)}));
        out.push(json!({"op": "mul_int", "a": a(x), "n": n, "out": get(p * n)}));
        out.push(json!({"op": "div_int", "a": a(x), "n": n, "out": get(p / n)}));
        out.push(json!({"op": "div_pow2_floor", "a": a(x), "k": k, "out": get(p.div_pow2_floor(k))}));
        out.push(json!({"op": "div_pow2_trunc", "a": a(x), "k": k, "out": get(p.div_pow2_trunc(k))}));
        out.push(json!({"op": "settle_to_zero", "a": a(x), "out": get(p.settle_to_zero())}));
        out.push(json!({"op": "snap_to_unit", "a": a(x), "out": get(p.snap_to_unit())}));
        out.push(json!({"op": "floor_int", "a": a(x), "out": p.floor_int()}));
        out.push(json!({"op": "trunc_int", "a": a(x), "out": p.trunc_int()}));
        out.push(json!({"op": "abs", "a": a(x), "out": get(p.abs())}));
        // An angle is a fraction of a turn.
        if F > 0 {
            let angle = Angle::<Exact, W, F>::from_turns(p);
            out.push(json!({"op": "normalized", "a": a(x), "out": get(angle.normalized().turns())}));
            out.push(json!({"op": "normalized_signed", "a": a(x), "out": get(angle.normalized_signed().turns())}));
        }
    }
    // The divisions' edges: the most negative over -1, and zero divisors.
    for (x, n) in [(W::MIN, -1), (W::MIN, 0), (-1, 0), (0, 0), (1, 0), (W::MAX, 0)] {
        out.push(json!({"op": "div_int", "a": a(x), "n": n, "out": get(fx::<W, F>(x) / n)}));
    }
}

/// The operations on two formats: `a`'s (W, F) and `b`'s (W2, G).
fn binary<W: Word, const F: u32, W2: Word, const G: u32>(rng: &mut Rng, out: &mut Vec<Value>) {
    let (xs, ys) = (cases::<W, F>(rng), cases::<W2, G>(rng));
    for (i, &x) in xs.iter().enumerate() {
        for &y in [ys[i % ys.len()], ys[(i * 5 + 1) % ys.len()]].iter() {
            let (p, q) = (fx::<W, F>(x), fx::<W2, G>(y));
            let (a, b) = (num::<W, F>(x), num::<W2, G>(y));
            out.push(json!({"op": "mul", "a": a, "b": b, "out": get(p * q)}));
            out.push(json!({"op": "mul_wide", "a": a, "b": b, "out": get(p.mul_wide(q))}));
            out.push(json!({"op": "div", "a": a, "b": b, "out": get(p / q)}));
        }
        if i < 6 {
            // Over zero and over minus one unit, from each edge.
            for y in [0, -(1i64 << G)].into_iter().filter(|y| (W2::MIN..=W2::MAX).contains(y)) {
                let (p, q) = (fx::<W, F>(x), fx::<W2, G>(y));
                out.push(json!({"op": "div", "a": num::<W, F>(x), "b": num::<W2, G>(y), "out": get(p / q)}));
            }
        }
        let r: Fixed<W2, G> = fx::<W, F>(x).rescale();
        out.push(json!({"op": "rescale", "a": num::<W, F>(x), "to": [word::<W2>(), G], "out": get(r)}));
    }
}

/// `sin_quarter` and `cos_quarter` with the SIN_QUARTER table (12 bits a
/// turn).
fn angles<W: Word>(rng: &mut Rng, out: &mut Vec<Value>) {
    let mut xs = edges::<W, 12>();
    xs.extend([1024, 2048, 3072, 4096, 4095, 1023, 1025, -1024, -4097].iter().filter(|x| **x <= W::MAX));
    xs.extend((0..4 * RANDOM).map(|_| rng.raw::<W>()));
    for x in xs {
        let a = Angle::<Exact, W, 12>::from_units(x);
        let (s, c) = (a.sin_quarter::<12>(&math::SIN_QUARTER), a.cos_quarter::<12>(&math::SIN_QUARTER));
        out.push(json!({"op": "sin_quarter", "a": num::<W, 12>(x), "out": get(s)}));
        out.push(json!({"op": "cos_quarter", "a": num::<W, 12>(x), "out": get(c)}));
    }
}

fn fixed() -> Vec<Value> {
    let mut rng = Rng(0x7073_782d_6669_7865);
    let mut out = Vec::new();
    let r = &mut rng;
    unary::<i8, 4>(r, &mut out);
    unary::<u8, 4>(r, &mut out);
    unary::<i16, 8>(r, &mut out);
    unary::<i16, 12>(r, &mut out);
    unary::<u16, 8>(r, &mut out);
    unary::<i32, 0>(r, &mut out);
    unary::<i32, 8>(r, &mut out);
    unary::<i32, 12>(r, &mut out);
    unary::<i32, 16>(r, &mut out);
    unary::<u32, 12>(r, &mut out);
    unary::<u32, 16>(r, &mut out);
    binary::<i32, 12, i32, 12>(r, &mut out);
    binary::<i32, 12, i16, 12>(r, &mut out);
    binary::<i32, 16, i32, 16>(r, &mut out);
    binary::<i32, 12, i32, 16>(r, &mut out);
    binary::<i32, 16, i32, 12>(r, &mut out);
    binary::<i16, 8, i16, 8>(r, &mut out);
    binary::<i16, 12, i32, 0>(r, &mut out);
    binary::<i32, 12, u32, 12>(r, &mut out);
    binary::<u32, 16, i32, 8>(r, &mut out);
    binary::<u16, 8, i32, 12>(r, &mut out);
    binary::<i8, 4, u8, 4>(r, &mut out);
    binary::<i32, 8, u8, 0>(r, &mut out);
    binary::<i32, 0, i32, 0>(r, &mut out);
    angles::<i16>(r, &mut out);
    angles::<i32>(r, &mut out);
    out
}

/// The data registers a vector writes: all but SXYP (15, which pushes the
/// screen FIFO), IRGB and ORGB (28, 29, which set or mirror IR1-3) and LZCR
/// (31, read only). FLAG (control 31) is left too: a command clears it.
fn written(data: bool, i: usize) -> bool {
    if data { !matches!(i, 15 | 28 | 29 | 31) } else { i != 31 }
}

fn gte() -> Vec<Value> {
    use psx_gte::op;
    let mut rng = Rng(0x7073_782d_6774_6531);
    let mut out = Vec::new();
    const OPS: [u32; 22] = [
        op::RTPS,
        op::NCLIP,
        op::OP,
        op::DPCS,
        op::INTPL,
        op::MVMVA,
        op::NCDS,
        op::CDP,
        op::NCDT,
        op::NCCS,
        op::CC,
        op::NCS,
        op::NCT,
        op::SQR,
        op::DCPL,
        op::DPCT,
        op::AVSZ3,
        op::AVSZ4,
        op::RTPT,
        op::GPF,
        op::GPL,
        op::NCCT,
    ];
    for opcode in OPS {
        for (sf, lm) in [(false, false), (true, false), (false, true), (true, true)] {
            for case in 0..GTE_CASES {
                let command = if opcode == op::MVMVA {
                    let r = rng.next();
                    op::mvmva(sf, lm, (r & 3) as u32, ((r >> 2) & 3) as u32, ((r >> 4) & 3) as u32)
                } else {
                    op::encode(opcode, sf, lm)
                };
                // Small values first, where a game's numbers are; then any.
                let small = case < GTE_CASES * 3 / 4;
                let reg = |rng: &mut Rng| -> u32 {
                    if small {
                        let h = |r: u64| ((r % 0x2000) as i32 - 0x1000) as i16 as u16 as u32;
                        h(rng.next()) | h(rng.next()) << 16
                    } else {
                        rng.next() as u32
                    }
                };
                let mut g = Gte::new();
                let (mut data, mut ctrl) = (vec![Value::Null; 32], vec![Value::Null; 32]);
                for i in (0..32).filter(|i| written(true, *i)) {
                    let v = reg(&mut rng);
                    g.write_data(i, v);
                    data[i] = json!(v);
                }
                for i in (0..32).filter(|i| written(false, *i)) {
                    let v = reg(&mut rng);
                    g.write_ctrl(i, v);
                    ctrl[i] = json!(v);
                }
                g.execute(command);
                let after: Vec<u32> = (0..32).map(|i| g.read_data(i)).chain((0..32).map(|i| g.read_ctrl(i))).collect();
                out.push(json!({"op": "execute", "command": command, "data": data, "ctrl": ctrl, "out": after}));
            }
        }
    }
    let matrix = |rng: &mut Rng| {
        let mut m = [[0i16; 3]; 3];
        for e in m.iter_mut().flatten() {
            *e = (rng.next() % 0x2001) as i16 - 0x1000;
        }
        Matrix::new(m)
    };
    let vector = |rng: &mut Rng| Vec3i16::new(rng.next() as i16, rng.next() as i16, rng.next() as i16);
    let rows = |m: &Matrix| json!(m.m);
    for _ in 0..4 * RANDOM {
        let (a, b, v) = (matrix(&mut rng), matrix(&mut rng), vector(&mut rng));
        let r = Vec3i16::new(rng.next() as i16, rng.next() as i16, rng.next() as i16);
        out.push(json!({"op": "mul_matrix", "a": rows(&a), "b": rows(&b), "out": rows(&math::mul_matrix(&a, &b))}));
        let am = math::apply_matrix(&a, v);
        out.push(json!({"op": "apply_matrix", "m": rows(&a), "v": v.to_array(), "out": [am.x, am.y, am.z]}));
        out.push(json!({"op": "apply_matrix_sv", "m": rows(&a), "v": v.to_array(),
                        "out": math::apply_matrix_sv(&a, v).to_array()}));
        out.push(json!({"op": "rot_matrix", "r": r.to_array(), "out": rows(&math::rot_matrix(r))}));
        let (h, sz3) = (rng.next() as u16, (rng.next() >> (rng.next() % 64)) as u16);
        let (q, overflow) = psx_gte::unr_divide(h, sz3);
        out.push(json!({"op": "unr_divide", "h": h, "sz3": sz3, "out": [q, overflow]}));
        let lzcs = (rng.next() as u32) >> (rng.next() % 32);
        let lzcs = if rng.next().is_multiple_of(2) { lzcs } else { !lzcs };
        out.push(json!({"op": "lzcr", "lzcs": lzcs, "out": psx_gte::lzcr(lzcs)}));
    }
    for a in (-4096..8192).step_by(37) {
        out.push(json!({"op": "rsin", "a": a, "out": math::rsin(a)}));
        out.push(json!({"op": "rcos", "a": a, "out": math::rcos(a)}));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_vectors_are_the_same_every_time() {
        let (a, b) = (fixed(), fixed());
        assert_eq!(a, b);
        assert_eq!(gte(), gte());
        // The edges are there: a zero divisor, and the most negative over -1.
        let has = |op: &str, x: i64, n: i64| {
            a.iter().any(|v| v["op"] == op && v["a"][0] == "i32" && v["a"][2] == x && v["n"] == n)
        };
        assert!(has("div_int", 1, 0) && has("div_int", i32::MIN as i64, -1));
    }
}
