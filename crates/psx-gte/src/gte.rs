//! The GTE register file, command execution and typed helpers.

use crate::types::{Matrix, Rgbc, ScreenXY, Vec3i16, Vec3i32};
use crate::unr::{lzcr, unr_divide};

/// FLAG register (control register 63) bits.
pub mod flag {
    pub const MAC1_POS: u32 = 1 << 30;
    pub const MAC2_POS: u32 = 1 << 29;
    pub const MAC3_POS: u32 = 1 << 28;
    pub const MAC1_NEG: u32 = 1 << 27;
    pub const MAC2_NEG: u32 = 1 << 26;
    pub const MAC3_NEG: u32 = 1 << 25;
    pub const IR1_SAT: u32 = 1 << 24;
    pub const IR2_SAT: u32 = 1 << 23;
    pub const IR3_SAT: u32 = 1 << 22;
    pub const COLOR_R_SAT: u32 = 1 << 21;
    pub const COLOR_G_SAT: u32 = 1 << 20;
    pub const COLOR_B_SAT: u32 = 1 << 19;
    pub const SZ_OTZ_SAT: u32 = 1 << 18;
    pub const DIV_OVERFLOW: u32 = 1 << 17;
    pub const MAC0_POS: u32 = 1 << 16;
    pub const MAC0_NEG: u32 = 1 << 15;
    pub const SX2_SAT: u32 = 1 << 14;
    pub const SY2_SAT: u32 = 1 << 13;
    pub const IR0_SAT: u32 = 1 << 12;
    /// Bit 31: OR of bits 30-23 and 18-13 (computed on read).
    pub const ERROR: u32 = 1 << 31;
    pub const ERROR_MASK: u32 = 0x7F87_E000;
    /// Bits that are writable / stored.
    pub const WRITABLE: u32 = 0x7FFF_F000;
}

/// Command opcodes (low 6 bits of the command word) and encoders.
pub mod op {
    pub const RTPS: u32 = 0x01;
    pub const NCLIP: u32 = 0x06;
    pub const OP: u32 = 0x0C;
    pub const DPCS: u32 = 0x10;
    pub const INTPL: u32 = 0x11;
    pub const MVMVA: u32 = 0x12;
    pub const NCDS: u32 = 0x13;
    pub const CDP: u32 = 0x14;
    pub const NCDT: u32 = 0x16;
    pub const NCCS: u32 = 0x1B;
    pub const CC: u32 = 0x1C;
    pub const NCS: u32 = 0x1E;
    pub const NCT: u32 = 0x20;
    pub const SQR: u32 = 0x28;
    pub const DCPL: u32 = 0x29;
    pub const DPCT: u32 = 0x2A;
    pub const AVSZ3: u32 = 0x2D;
    pub const AVSZ4: u32 = 0x2E;
    pub const RTPT: u32 = 0x30;
    pub const GPF: u32 = 0x3D;
    pub const GPL: u32 = 0x3E;
    pub const NCCT: u32 = 0x3F;

    /// Command word: `sf` = bit 19 (shift MAC by 12), `lm` = bit 10 (clamp
    /// IR1-3 to 0..). Bits 20-24 (ignored by hardware) are left zero.
    pub const fn encode(opcode: u32, sf: bool, lm: bool) -> u32 {
        (sf as u32) << 19 | (lm as u32) << 10 | (opcode & 0x3F)
    }

    /// MVMVA command word. `mx`: 0=RT, 1=LLM, 2=LCM, 3=garbage.
    /// `v`: 0-2=V0-V2, 3=IR. `cv`: 0=TR, 1=BK, 2=FC (buggy), 3=none.
    pub const fn mvmva(sf: bool, lm: bool, mx: u32, v: u32, cv: u32) -> u32 {
        encode(MVMVA, sf, lm) | (mx & 3) << 17 | (v & 3) << 15 | (cv & 3) << 13
    }
}

const MAC_POS: [u32; 4] = [
    flag::MAC0_POS,
    flag::MAC1_POS,
    flag::MAC2_POS,
    flag::MAC3_POS,
];
const MAC_NEG: [u32; 4] = [
    flag::MAC0_NEG,
    flag::MAC1_NEG,
    flag::MAC2_NEG,
    flag::MAC3_NEG,
];
const IR_SAT: [u32; 4] = [flag::IR0_SAT, flag::IR1_SAT, flag::IR2_SAT, flag::IR3_SAT];
const COLOR_SAT: [u32; 3] = [flag::COLOR_R_SAT, flag::COLOR_G_SAT, flag::COLOR_B_SAT];

const MAC_MAX: i64 = (1 << 43) - 1;
const MAC_MIN: i64 = -(1 << 43);

fn wrap44(v: i64) -> i64 {
    (v << 20) >> 20
}

/// 44-bit MAC1-3 accumulator: every addition wraps and records overflow.
#[derive(Clone, Copy)]
struct Acc {
    val: i64,
    pos: bool,
    neg: bool,
}

impl Acc {
    fn new(v: i64) -> Self {
        Acc {
            val: wrap44(v),
            pos: v > MAC_MAX,
            neg: v < MAC_MIN,
        }
    }

    fn add(self, x: i64) -> Self {
        let r = self.val + x;
        Acc {
            val: wrap44(r),
            pos: self.pos || r > MAC_MAX,
            neg: self.neg || r < MAC_MIN,
        }
    }
}

/// Decoded `sf`/`lm` bits.
#[derive(Clone, Copy)]
struct Mode {
    sf: bool,
    lm: bool,
}

/// Result of [`Gte::rtps`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RtpsResult {
    /// Projected screen coordinate (SXY2).
    pub sxy: ScreenXY,
    /// Screen Z (SZ3), 0..=0xFFFF.
    pub sz: u16,
    /// Depth-cue interpolation factor (IR0), 0..=0x1000.
    pub ir0: i16,
    /// FLAG register after the command (bit 31 included).
    pub flags: u32,
}

/// Result of [`Gte::rtpt`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RtptResult {
    /// SXY0-2 (one per input vertex).
    pub sxy: [ScreenXY; 3],
    /// SZ1-3 (one per input vertex).
    pub sz: [u16; 3],
    /// Depth-cue factor of the last vertex (IR0).
    pub ir0: i16,
    pub flags: u32,
}

/// The complete GTE state: 32 data registers (cop2r0-31) and 32 control
/// registers (cop2r32-63), stored in decoded form.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Gte {
    // Data registers.
    v: [[i16; 3]; 3],
    rgbc: [u8; 4],
    otz: u16,
    ir: [i16; 4],
    sxy: [u32; 3],
    sz: [u16; 4],
    rgb: [u32; 3],
    res1: u32,
    mac: [i32; 4],
    lzcs: u32,
    // Control registers.
    rt: [[i16; 3]; 3],
    tr: [i32; 3],
    llm: [[i16; 3]; 3],
    bk: [i32; 3],
    lcm: [[i16; 3]; 3],
    fc: [i32; 3],
    ofx: i32,
    ofy: i32,
    h: u16,
    dqa: i16,
    dqb: i32,
    zsf3: i16,
    zsf4: i16,
    flag: u32,
}

const fn lo(v: u32) -> i16 {
    v as i16
}
const fn hi(v: u32) -> i16 {
    (v >> 16) as i16
}
const fn pack(l: i16, h: i16) -> u32 {
    l as u16 as u32 | (h as u16 as u32) << 16
}
const fn sext(v: i16) -> u32 {
    v as i32 as u32
}

/// Read 32-bit word `k` (0..=4) of a packed 3x3 matrix.
fn mat_read(m: &[[i16; 3]; 3], k: usize) -> u32 {
    let e = |i: usize| m[i / 3][i % 3];
    if k == 4 {
        sext(e(8))
    } else {
        pack(e(2 * k), e(2 * k + 1))
    }
}

fn mat_write(m: &mut [[i16; 3]; 3], k: usize, v: u32) {
    let mut set = |i: usize, x: i16| m[i / 3][i % 3] = x;
    set(2 * k, lo(v));
    if k < 4 {
        set(2 * k + 1, hi(v));
    }
}

impl Gte {
    pub fn new() -> Self {
        Self::default()
    }

    // ---------------------------------------------------------------------
    // Register transfer (MFC2/MTC2/CFC2/CTC2, LWC2/SWC2)
    // ---------------------------------------------------------------------

    /// Read data register `index` (0..=31), as MFC2/SWC2 would.
    pub fn read_data(&self, index: usize) -> u32 {
        match index & 31 {
            i @ (0 | 2 | 4) => {
                let v = self.v[i / 2];
                pack(v[0], v[1])
            }
            i @ (1 | 3 | 5) => sext(self.v[i / 2][2]),
            6 => u32::from_le_bytes(self.rgbc),
            7 => self.otz as u32,
            i @ 8..=11 => sext(self.ir[i - 8]),
            i @ 12..=14 => self.sxy[i - 12],
            15 => self.sxy[2],
            i @ 16..=19 => self.sz[i - 16] as u32,
            i @ 20..=22 => self.rgb[i - 20],
            23 => self.res1,
            i @ 24..=27 => self.mac[i - 24] as u32,
            28 | 29 => self.orgb(),
            30 => self.lzcs,
            _ => lzcr(self.lzcs),
        }
    }

    /// Write data register `index` (0..=31), as MTC2/LWC2 would.
    pub fn write_data(&mut self, index: usize, value: u32) {
        match index & 31 {
            i @ (0 | 2 | 4) => {
                self.v[i / 2][0] = lo(value);
                self.v[i / 2][1] = hi(value);
            }
            i @ (1 | 3 | 5) => self.v[i / 2][2] = lo(value),
            6 => self.rgbc = value.to_le_bytes(),
            7 => self.otz = value as u16,
            i @ 8..=11 => self.ir[i - 8] = lo(value),
            i @ 12..=14 => self.sxy[i - 12] = value,
            15 => self.push_sxy(value),
            i @ 16..=19 => self.sz[i - 16] = value as u16,
            i @ 20..=22 => self.rgb[i - 20] = value,
            23 => self.res1 = value,
            i @ 24..=27 => self.mac[i - 24] = value as i32,
            28 => {
                // IRGB: 5:5:5 -> IR1-3 = component * 0x80.
                self.ir[1] = ((value & 0x1F) << 7) as i16;
                self.ir[2] = ((value >> 5 & 0x1F) << 7) as i16;
                self.ir[3] = ((value >> 10 & 0x1F) << 7) as i16;
            }
            30 => self.lzcs = value,
            _ => {} // 29 ORGB and 31 LZCR are read-only.
        }
    }

    /// Read control register `index` (0..=31, i.e. cop2r32-63), as CFC2.
    pub fn read_ctrl(&self, index: usize) -> u32 {
        match index & 31 {
            i @ 0..=4 => mat_read(&self.rt, i),
            i @ 5..=7 => self.tr[i - 5] as u32,
            i @ 8..=12 => mat_read(&self.llm, i - 8),
            i @ 13..=15 => self.bk[i - 13] as u32,
            i @ 16..=20 => mat_read(&self.lcm, i - 16),
            i @ 21..=23 => self.fc[i - 21] as u32,
            24 => self.ofx as u32,
            25 => self.ofy as u32,
            // Hardware bug: H is unsigned but reads back sign-extended.
            26 => sext(self.h as i16),
            27 => sext(self.dqa),
            28 => self.dqb as u32,
            29 => sext(self.zsf3),
            30 => sext(self.zsf4),
            _ => self.flag(),
        }
    }

    /// Write control register `index` (0..=31, i.e. cop2r32-63), as CTC2.
    pub fn write_ctrl(&mut self, index: usize, value: u32) {
        match index & 31 {
            i @ 0..=4 => mat_write(&mut self.rt, i, value),
            i @ 5..=7 => self.tr[i - 5] = value as i32,
            i @ 8..=12 => mat_write(&mut self.llm, i - 8, value),
            i @ 13..=15 => self.bk[i - 13] = value as i32,
            i @ 16..=20 => mat_write(&mut self.lcm, i - 16, value),
            i @ 21..=23 => self.fc[i - 21] = value as i32,
            24 => self.ofx = value as i32,
            25 => self.ofy = value as i32,
            26 => self.h = value as u16,
            27 => self.dqa = lo(value),
            28 => self.dqb = value as i32,
            29 => self.zsf3 = lo(value),
            30 => self.zsf4 = lo(value),
            _ => self.flag = value & flag::WRITABLE,
        }
    }

    /// FLAG register with the bit-31 error summary.
    pub fn flag(&self) -> u32 {
        let err = if self.flag & flag::ERROR_MASK != 0 {
            flag::ERROR
        } else {
            0
        };
        self.flag | err
    }

    fn orgb(&self) -> u32 {
        let c = |x: i16| (x >> 7).clamp(0, 0x1F) as u32;
        c(self.ir[1]) | c(self.ir[2]) << 5 | c(self.ir[3]) << 10
    }

    fn push_sxy(&mut self, v: u32) {
        self.sxy = [self.sxy[1], self.sxy[2], v];
    }

    // ---------------------------------------------------------------------
    // Command execution
    // ---------------------------------------------------------------------

    /// Execute a COP2 command word (only the low 25 bits are decoded).
    ///
    /// Returns the command's documented cycle count, or `None` for an
    /// unassigned opcode (which leaves all registers untouched).
    pub fn execute(&mut self, cmd: u32) -> Option<u32> {
        let m = Mode {
            sf: cmd >> 19 & 1 != 0,
            lm: cmd >> 10 & 1 != 0,
        };
        let opcode = cmd & 0x3F;
        let cycles = match opcode {
            op::RTPS => 15,
            op::NCLIP => 8,
            op::OP => 6,
            op::DPCS | op::INTPL | op::MVMVA | op::DCPL => 8,
            op::NCDS => 19,
            op::CDP => 13,
            op::NCDT => 44,
            op::NCCS => 17,
            op::CC => 11,
            op::NCS => 14,
            op::NCT => 30,
            op::SQR | op::AVSZ3 | op::GPF | op::GPL => 5,
            op::DPCT => 17,
            op::AVSZ4 => 6,
            op::RTPT => 23,
            op::NCCT => 39,
            _ => return None,
        };
        self.flag = 0;
        match opcode {
            op::RTPS => self.rtp(m, 0, true),
            op::RTPT => {
                self.rtp(m, 0, false);
                self.rtp(m, 1, false);
                self.rtp(m, 2, true);
            }
            op::NCLIP => self.cmd_nclip(),
            op::OP => self.cmd_op(m),
            op::DPCS => {
                let c = self.rgbc;
                self.depth_cue(m, [0, 1, 2].map(|i| (c[i] as i64) << 16));
                self.push_color();
            }
            op::INTPL => {
                let ir = self.ir;
                self.depth_cue(m, [1, 2, 3].map(|i| (ir[i] as i64) << 12));
                self.push_color();
            }
            op::MVMVA => self.cmd_mvmva(m, cmd),
            op::NCDS => self.ncd(m, 0),
            op::NCDT => (0..3).for_each(|i| self.ncd(m, i)),
            op::CDP => {
                self.color_matrix(m);
                self.depth_cue_color(m);
                self.push_color();
            }
            op::NCCS => self.ncc(m, 0),
            op::NCCT => (0..3).for_each(|i| self.ncc(m, i)),
            op::CC => {
                self.color_matrix(m);
                self.color_apply(m);
                self.push_color();
            }
            op::NCS => self.nc(m, 0),
            op::NCT => (0..3).for_each(|i| self.nc(m, i)),
            op::SQR => {
                let ir = self.ir;
                self.set_macs(m, [1, 2, 3].map(|i| Acc::new(ir[i] as i64 * ir[i] as i64)));
            }
            op::DCPL => {
                self.depth_cue_color(m);
                self.push_color();
            }
            op::DPCT => {
                for _ in 0..3 {
                    let c = self.rgb[0].to_le_bytes();
                    self.depth_cue(m, [0, 1, 2].map(|i| (c[i] as i64) << 16));
                    self.push_color();
                }
            }
            op::AVSZ3 => {
                let s = self.sz[1] as i64 + self.sz[2] as i64 + self.sz[3] as i64;
                self.cmd_avsz(self.zsf3 as i64 * s);
            }
            op::AVSZ4 => {
                let s: i64 = self.sz.iter().map(|&z| z as i64).sum();
                self.cmd_avsz(self.zsf4 as i64 * s);
            }
            op::GPF => {
                let (ir, ir0) = (self.ir, self.ir[0] as i64);
                self.set_macs(m, [1, 2, 3].map(|i| Acc::new(ir0 * ir[i] as i64)));
                self.push_color();
            }
            op::GPL => {
                let (ir, ir0, mac) = (self.ir, self.ir[0] as i64, self.mac);
                let sh = if m.sf { 12 } else { 0 };
                self.set_macs(
                    m,
                    [1, 2, 3].map(|i| Acc::new(((mac[i] as i64) << sh) + ir0 * ir[i] as i64)),
                );
                self.push_color();
            }
            _ => unreachable!(),
        }
        Some(cycles)
    }

    // ---------------------------------------------------------------------
    // Arithmetic building blocks
    // ---------------------------------------------------------------------

    /// Flag MAC1-3 overflow and return the shifted, 32-bit truncated value.
    fn mac_value(&mut self, i: usize, a: Acc, sf: bool) -> i32 {
        if a.pos {
            self.flag |= MAC_POS[i];
        }
        if a.neg {
            self.flag |= MAC_NEG[i];
        }
        (if sf { a.val >> 12 } else { a.val }) as i32
    }

    /// Flag MAC0 (32-bit) overflow and return the truncated value.
    fn mac0_value(&mut self, v: i64) -> i32 {
        if v > i32::MAX as i64 {
            self.flag |= flag::MAC0_POS;
        } else if v < i32::MIN as i64 {
            self.flag |= flag::MAC0_NEG;
        }
        v as i32
    }

    /// Saturate into IR1-3 range (`lm` selects the 0 lower bound).
    fn lim_b(&mut self, i: usize, v: i32, lm: bool) -> i16 {
        let min = if lm { 0 } else { -0x8000 };
        if v < min || v > 0x7FFF {
            self.flag |= IR_SAT[i];
        }
        v.clamp(min, 0x7FFF) as i16
    }

    /// Saturate to 0..=0xFFFF (SZ3 / OTZ).
    fn lim_d(&mut self, v: i64) -> u16 {
        if !(0..=0xFFFF).contains(&v) {
            self.flag |= flag::SZ_OTZ_SAT;
        }
        v.clamp(0, 0xFFFF) as u16
    }

    /// Store MAC1-3 from accumulators, then IR1-3 = Lm_B(MAC).
    fn set_macs(&mut self, m: Mode, acc: [Acc; 3]) {
        for (i, a) in acc.into_iter().enumerate() {
            self.mac[i + 1] = self.mac_value(i + 1, a, m.sf);
        }
        self.ir_from_mac(m.lm);
    }

    fn ir_from_mac(&mut self, lm: bool) {
        for i in 1..4 {
            self.ir[i] = self.lim_b(i, self.mac[i], lm);
        }
    }

    /// MAC1-3 = (T*0x1000 + M*V) >> (sf*12), with the far-color hardware bug
    /// when `fc_bug` is set. Does not touch IR. Returns the raw 44-bit MACs.
    fn mat_vec(
        &mut self,
        sf: bool,
        mx: [[i64; 3]; 3],
        v: [i64; 3],
        t: [i64; 3],
        fc_bug: bool,
    ) -> [i64; 3] {
        let mut raw = [0; 3];
        for i in 0..3 {
            let row = mx[i];
            let a = if fc_bug {
                // The first column (with FC) is computed only for its flags.
                let junk = Acc::new(t[i] << 12).add(row[0] * v[0]);
                let junk = self.mac_value(i + 1, junk, sf);
                self.lim_b(i + 1, junk, false);
                Acc::new(row[1] * v[1]).add(row[2] * v[2])
            } else {
                Acc::new(t[i] << 12)
                    .add(row[0] * v[0])
                    .add(row[1] * v[1])
                    .add(row[2] * v[2])
            };
            self.mac[i + 1] = self.mac_value(i + 1, a, sf);
            raw[i] = a.val;
        }
        raw
    }

    fn vertex(&self, i: usize) -> [i64; 3] {
        if i < 3 {
            self.v[i].map(i64::from)
        } else {
            [self.ir[1], self.ir[2], self.ir[3]].map(i64::from)
        }
    }

    /// One RTPS step for vertex `vi`; `last` also computes depth cue (IR0).
    fn rtp(&mut self, m: Mode, vi: usize, last: bool) {
        let rt = widen(&self.rt);
        let raw = self.mat_vec(m.sf, rt, self.vertex(vi), self.tr.map(i64::from), false);
        self.ir[1] = self.lim_b(1, self.mac[1], m.lm);
        self.ir[2] = self.lim_b(2, self.mac[2], m.lm);
        // IR3: value saturates per lm from MAC3, but the flag is raised
        // only when MAC3 >> 12 leaves -0x8000..=0x7FFF (as if sf=1, lm=0).
        let min = if m.lm { 0 } else { -0x8000 };
        if !(-0x8000..=0x7FFF).contains(&(raw[2] >> 12)) {
            self.flag |= flag::IR3_SAT;
        }
        self.ir[3] = self.mac[3].clamp(min, 0x7FFF) as i16;

        let z = self.lim_d(raw[2] >> 12);
        self.sz = [self.sz[1], self.sz[2], self.sz[3], z];

        let (q, ovf) = unr_divide(self.h, z);
        if ovf {
            self.flag |= flag::DIV_OVERFLOW;
        }
        let q = q as i64;
        let sx = self.ofx as i64 + self.ir[1] as i64 * q;
        let sy = self.ofy as i64 + self.ir[2] as i64 * q;
        self.mac0_value(sx);
        self.mac0_value(sy);
        let sx = lim_g(&mut self.flag, sx >> 16, flag::SX2_SAT);
        let sy = lim_g(&mut self.flag, sy >> 16, flag::SY2_SAT);
        self.push_sxy(pack(sx, sy));

        if last {
            let d = self.dqb as i64 + self.dqa as i64 * q;
            self.mac[0] = self.mac0_value(d);
            let ir0 = d >> 12;
            if !(0..=0x1000).contains(&ir0) {
                self.flag |= flag::IR0_SAT;
            }
            self.ir[0] = ir0.clamp(0, 0x1000) as i16;
        }
    }

    fn cmd_nclip(&mut self) {
        let [s0, s1, s2] = self.sxy.map(|v| (lo(v) as i64, hi(v) as i64));
        let n = s0.0 * s1.1 + s1.0 * s2.1 + s2.0 * s0.1 - s0.0 * s2.1 - s1.0 * s0.1 - s2.0 * s1.1;
        self.mac[0] = self.mac0_value(n);
    }

    fn cmd_op(&mut self, m: Mode) {
        let d = [self.rt[0][0], self.rt[1][1], self.rt[2][2]].map(i64::from);
        let ir = self.ir.map(i64::from);
        self.set_macs(
            m,
            [
                Acc::new(d[1] * ir[3] - d[2] * ir[2]),
                Acc::new(d[2] * ir[1] - d[0] * ir[3]),
                Acc::new(d[0] * ir[2] - d[1] * ir[1]),
            ],
        );
    }

    fn cmd_avsz(&mut self, v: i64) {
        self.mac[0] = self.mac0_value(v);
        self.otz = self.lim_d(v >> 12);
    }

    fn cmd_mvmva(&mut self, m: Mode, cmd: u32) {
        let mx = match cmd >> 17 & 3 {
            0 => widen(&self.rt),
            1 => widen(&self.llm),
            2 => widen(&self.lcm),
            _ => {
                // Undefined "garbage" matrix observed on hardware.
                let r = self.rgbc[0] as i64;
                let (r13, r22, ir0) = (
                    self.rt[0][2] as i64,
                    self.rt[1][1] as i64,
                    self.ir[0] as i64,
                );
                [[-(r << 4), r << 4, ir0], [r13; 3], [r22; 3]]
            }
        };
        let v = self.vertex((cmd >> 15 & 3) as usize);
        let cv = cmd >> 13 & 3;
        let t = match cv {
            0 => self.tr,
            1 => self.bk,
            2 => self.fc,
            _ => [0; 3],
        };
        self.mat_vec(m.sf, mx, v, t.map(i64::from), cv == 2);
        self.ir_from_mac(m.lm);
    }

    // Color pipeline stages.

    /// MAC/IR = LLM * V[vi].
    fn light(&mut self, m: Mode, vi: usize) {
        let llm = widen(&self.llm);
        self.mat_vec(m.sf, llm, self.vertex(vi), [0; 3], false);
        self.ir_from_mac(m.lm);
    }

    /// MAC/IR = BK * 0x1000 + LCM * IR.
    fn color_matrix(&mut self, m: Mode) {
        let lcm = widen(&self.lcm);
        self.mat_vec(m.sf, lcm, self.vertex(3), self.bk.map(i64::from), false);
        self.ir_from_mac(m.lm);
    }

    /// MAC = input + (FC*0x1000 - input) * IR0, IR = Lm_B(MAC).
    fn depth_cue(&mut self, m: Mode, input: [i64; 3]) {
        let ir0 = self.ir[0] as i64;
        for (i, inp) in input.into_iter().enumerate() {
            let t = Acc::new(((self.fc[i] as i64) << 12) - inp);
            let t = self.mac_value(i + 1, t, m.sf);
            let t = self.lim_b(i + 1, t, false) as i64;
            let a = Acc::new(inp + ir0 * t);
            self.mac[i + 1] = self.mac_value(i + 1, a, m.sf);
        }
        self.ir_from_mac(m.lm);
    }

    /// Depth cue of `RGBC * IR` (the second half of NCDS/CDP/DCPL).
    fn depth_cue_color(&mut self, m: Mode) {
        let input = [0, 1, 2].map(|i| ((self.rgbc[i] as i64) << 4) * self.ir[i + 1] as i64);
        self.depth_cue(m, input);
    }

    /// MAC/IR = RGBC * IR.
    fn color_apply(&mut self, m: Mode) {
        let input =
            [0, 1, 2].map(|i| Acc::new(((self.rgbc[i] as i64) << 4) * self.ir[i + 1] as i64));
        self.set_macs(m, input);
    }

    /// Push MAC1-3 / 16 (saturated to u8) + RGBC.code onto the RGB FIFO.
    fn push_color(&mut self) {
        let mut c = [0u8, 0, 0, self.rgbc[3]];
        for i in 0..3 {
            let v = self.mac[i + 1] >> 4;
            if !(0..=0xFF).contains(&v) {
                self.flag |= COLOR_SAT[i];
            }
            c[i] = v.clamp(0, 0xFF) as u8;
        }
        self.rgb = [self.rgb[1], self.rgb[2], u32::from_le_bytes(c)];
    }

    fn ncd(&mut self, m: Mode, vi: usize) {
        self.light(m, vi);
        self.color_matrix(m);
        self.depth_cue_color(m);
        self.push_color();
    }

    fn ncc(&mut self, m: Mode, vi: usize) {
        self.light(m, vi);
        self.color_matrix(m);
        self.color_apply(m);
        self.push_color();
    }

    fn nc(&mut self, m: Mode, vi: usize) {
        self.light(m, vi);
        self.color_matrix(m);
        self.push_color();
    }

    // ---------------------------------------------------------------------
    // Ergonomic layer for native code
    // ---------------------------------------------------------------------

    /// Rotation matrix RT (cop2r32-36).
    pub fn set_rotation(&mut self, m: &Matrix) {
        self.rt = m.m;
    }

    pub fn rotation(&self) -> Matrix {
        Matrix::new(self.rt)
    }

    /// Translation vector TR (cop2r37-39), integer units.
    pub fn set_translation(&mut self, t: Vec3i32) {
        self.tr = t.to_array();
    }

    pub fn translation(&self) -> Vec3i32 {
        Vec3i32::from_array(self.tr)
    }

    /// Light direction matrix LLM: one light direction per row (`1.3.12`).
    pub fn set_light_matrix(&mut self, m: &Matrix) {
        self.llm = m.m;
    }

    /// Light color matrix LCM: one light's color per column (`1.3.12`).
    pub fn set_color_matrix(&mut self, m: &Matrix) {
        self.lcm = m.m;
    }

    /// Background (ambient) color BK, `1.19.12` (0x1000 = 1.0 = full 255).
    pub fn set_back_color(&mut self, c: Vec3i32) {
        self.bk = c.to_array();
    }

    /// Far color FC used by depth cueing, `1.27.4`: pass `rgb8 << 4`
    /// (libgte `SetFarColor(r, g, b)` does exactly that).
    pub fn set_far_color(&mut self, c: Vec3i32) {
        self.fc = c.to_array();
    }

    /// Screen offset in whole pixels (like libgte `SetGeomOffset`);
    /// stored as `1.15.16` in OFX/OFY.
    pub fn set_screen_offset(&mut self, ofx: i32, ofy: i32) {
        self.ofx = ofx << 16;
        self.ofy = ofy << 16;
    }

    /// Raw OFX/OFY in `1.15.16` (sub-pixel offsets).
    pub fn set_screen_offset_raw(&mut self, ofx: i32, ofy: i32) {
        self.ofx = ofx;
        self.ofy = ofy;
    }

    /// Projection plane distance H (like libgte `SetGeomScreen`).
    pub fn set_projection_distance(&mut self, h: u16) {
        self.h = h;
    }

    /// Average-Z scale factors (`1.3.12`). `zsf3 = 0x1000 / 3` and
    /// `zsf4 = 0x1000 / 4` make OTZ the mean SZ; scale them down to fit a
    /// smaller ordering table.
    pub fn set_z_scale(&mut self, zsf3: i16, zsf4: i16) {
        self.zsf3 = zsf3;
        self.zsf4 = zsf4;
    }

    /// Depth-cue coefficients: `IR0 = (DQB + DQA * H/SZ) >> 12`.
    /// DQA is `1.7.8`, DQB is `1.7.24`.
    pub fn set_depth_cue(&mut self, dqa: i16, dqb: i32) {
        self.dqa = dqa;
        self.dqb = dqb;
    }

    fn set_vertex(&mut self, i: usize, v: Vec3i16) {
        self.v[i] = v.to_array();
    }

    /// Rotate, translate and perspective-project one vertex (RTPS, sf=1, lm=0).
    pub fn rtps(&mut self, v: Vec3i16) -> RtpsResult {
        self.set_vertex(0, v);
        self.execute(op::encode(op::RTPS, true, false));
        RtpsResult {
            sxy: ScreenXY::from_u32(self.sxy[2]),
            sz: self.sz[3],
            ir0: self.ir[0],
            flags: self.flag(),
        }
    }

    /// Project three vertices (RTPT, sf=1, lm=0). Leaves them in SXY0-2 so
    /// [`Gte::nclip`] and [`Gte::avsz3`] can follow.
    pub fn rtpt(&mut self, v: [Vec3i16; 3]) -> RtptResult {
        for (i, v) in v.into_iter().enumerate() {
            self.set_vertex(i, v);
        }
        self.execute(op::encode(op::RTPT, true, false));
        RtptResult {
            sxy: self.sxy.map(ScreenXY::from_u32),
            sz: [self.sz[1], self.sz[2], self.sz[3]],
            ir0: self.ir[0],
            flags: self.flag(),
        }
    }

    /// Signed doubled area of SXY0-2 (NCLIP). Positive = counter-clockwise
    /// in screen space (y down), i.e. front-facing for libgte conventions.
    pub fn nclip(&mut self) -> i32 {
        self.execute(op::encode(op::NCLIP, false, false));
        self.mac[0]
    }

    /// OTZ = ZSF3 * (SZ1+SZ2+SZ3) >> 12.
    pub fn avsz3(&mut self) -> u16 {
        self.execute(op::encode(op::AVSZ3, false, false));
        self.otz
    }

    /// OTZ = ZSF4 * (SZ0+SZ1+SZ2+SZ3) >> 12.
    pub fn avsz4(&mut self) -> u16 {
        self.execute(op::encode(op::AVSZ4, false, false));
        self.otz
    }

    fn color_cmd(&mut self, opcode: u32, normal: Vec3i16, color: Rgbc) -> Rgbc {
        self.set_vertex(0, normal);
        self.rgbc = color.to_u32().to_le_bytes();
        self.execute(op::encode(opcode, true, true));
        Rgbc::from_u32(self.rgb[2])
    }

    /// Normal color with depth cue (NCDS, sf=1, lm=1). `normal` is `1.3.12`.
    pub fn ncds(&mut self, normal: Vec3i16, color: Rgbc) -> Rgbc {
        self.color_cmd(op::NCDS, normal, color)
    }

    /// Normal color (NCCS, sf=1, lm=1): lighting modulated by `color`.
    pub fn nccs(&mut self, normal: Vec3i16, color: Rgbc) -> Rgbc {
        self.color_cmd(op::NCCS, normal, color)
    }

    /// Normal lighting only (NCS, sf=1, lm=1); `code` byte comes from `color`.
    pub fn ncs(&mut self, normal: Vec3i16, color: Rgbc) -> Rgbc {
        self.color_cmd(op::NCS, normal, color)
    }

    /// Current IR1-3 (saturated last result).
    pub fn ir(&self) -> Vec3i16 {
        Vec3i16::new(self.ir[1], self.ir[2], self.ir[3])
    }

    /// Current MAC1-3.
    pub fn mac(&self) -> Vec3i32 {
        Vec3i32::new(self.mac[1], self.mac[2], self.mac[3])
    }
}

fn widen(m: &[[i16; 3]; 3]) -> [[i64; 3]; 3] {
    m.map(|r| r.map(i64::from))
}

fn lim_g(flags: &mut u32, v: i64, bit: u32) -> i16 {
    if !(-0x400..=0x3FF).contains(&v) {
        *flags |= bit;
    }
    v.clamp(-0x400, 0x3FF) as i16
}
