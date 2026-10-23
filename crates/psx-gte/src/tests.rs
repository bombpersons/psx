//! Hand-computed tests. Expected values are derived from the psx-spx
//! formulas, not from another emulator.

use crate::math::*;
use crate::*;

const ERR: u32 = flag::ERROR;

fn mvmva(sf: bool, lm: bool, mx: u32, v: u32, cv: u32) -> u32 {
    op::mvmva(sf, lm, mx, v, cv)
}

/// RT = I, TR = (0,0,tz), H = 1000, offset (160,120), DQA = -128, DQB = 1.0.
fn camera(tz: i32) -> Gte {
    let mut g = Gte::new();
    g.set_rotation(&Matrix::IDENTITY);
    g.set_translation(Vec3i32::new(0, 0, tz));
    g.set_projection_distance(1000);
    g.set_screen_offset(160, 120);
    g.set_depth_cue(-128, 0x100_0000);
    g
}

// --- UNR division -----------------------------------------------------------

#[test]
fn unr_table_endpoints() {
    assert_eq!(UNR_TABLE[0], 0xFF);
    assert_eq!(UNR_TABLE[128], 0x54);
    assert_eq!(UNR_TABLE[256], 0);
}

#[test]
fn unr_known_values() {
    assert_eq!(unr_divide(1000, 1000), (0x10000, false));
    assert_eq!(unr_divide(1, 1), (0x10000, false));
    assert_eq!(unr_divide(300, 1000), (19661, false));
    assert_eq!(unr_divide(0, 5), (0, false));
    // Not exact: the reciprocal approximation shows through.
    assert_eq!(unr_divide(0xFFFF, 0xFFFF), (0xFFFF, false));
    assert_eq!(unr_divide(0x7FFF, 0x4000), (0x1FFFC, false));
}

#[test]
fn unr_overflow() {
    assert_eq!(unr_divide(2000, 1000), (0x1FFFF, true)); // h == 2*sz3
    assert_eq!(unr_divide(0xFFFF, 1), (0x1FFFF, true));
    assert!(!unr_divide(1999, 1000).1);
    // sz3 == 0 always overflows, even with h == 0.
    assert_eq!(unr_divide(0, 0), (0x1FFFF, true));
    assert_eq!(unr_divide(1, 0), (0x1FFFF, true));
}

#[test]
fn unr_close_to_ideal() {
    for sz in (1..=0xFFFFu32).step_by(7) {
        for h in [0, 1, sz / 3, sz, 2 * sz - 1] {
            if h > 0xFFFF {
                continue;
            }
            let (q, ovf) = unr_divide(h as u16, sz as u16);
            assert!(!ovf);
            let ideal = (h as f64) * 65536.0 / sz as f64;
            assert!((q as f64 - ideal).abs() < 3.0, "h={h} sz={sz} q={q}");
        }
    }
}

// --- Register transfer quirks -------------------------------------------------

#[test]
fn data_sign_and_zero_extension() {
    let mut g = Gte::new();
    g.write_data(1, 0x1234_8000);
    assert_eq!(g.read_data(1), 0xFFFF_8000);
    g.write_data(9, 0x0000_FFFF);
    assert_eq!(g.read_data(9), 0xFFFF_FFFF);
    g.write_data(7, 0xFFFF_FFFF);
    assert_eq!(g.read_data(7), 0xFFFF);
    g.write_data(17, 0xABCD_8001);
    assert_eq!(g.read_data(17), 0x8001);
    g.write_data(0, 0x8000_7FFF);
    assert_eq!(g.read_data(0), 0x8000_7FFF);
    assert_eq!(g.read_data(31), 0); // LZCR is 0 at power-on (Mednafen)
    g.write_data(31, 5); // and read-only
    assert_eq!(g.read_data(31), 0);
    g.write_data(30, 0);
    assert_eq!(g.read_data(31), 32);
}

#[test]
fn ctrl_sign_extension_and_h_quirk() {
    let mut g = Gte::new();
    g.write_ctrl(26, 0x0000_8000);
    assert_eq!(g.read_ctrl(26), 0xFFFF_8000); // H reads sign-extended...
    let mut h = g.clone();
    h.write_ctrl(26, 0x0000_7FFF);
    assert_eq!(h.read_ctrl(26), 0x7FFF);
    for r in [4, 12, 20, 27, 29, 30] {
        g.write_ctrl(r, 0x1234_FFFE);
        assert_eq!(g.read_ctrl(r), 0xFFFF_FFFE, "reg {r}");
    }
    g.write_ctrl(0, 0x2000_1000);
    g.write_ctrl(3, 0xFFFF_0001);
    let m = g.rotation().m;
    assert_eq!((m[0][0], m[0][1], m[2][0], m[2][1], m[2][2]), (0x1000, 0x2000, 1, -1, -2));
}

#[test]
fn h_is_unsigned_in_rtps() {
    // ...but is used unsigned: H=0x8000 vs SZ=0x8000 divides to 1.0.
    let mut g = camera(0x8000);
    g.write_ctrl(26, 0x8000);
    g.set_screen_offset(0, 0);
    let r = g.rtps(Vec3i16::new(100, 0, 0));
    assert_eq!(r.sxy, ScreenXY::new(100, 0));
}

#[test]
fn flag_register_bit31() {
    let mut g = Gte::new();
    g.write_ctrl(31, 0xFFFF_FFFF);
    assert_eq!(g.read_ctrl(31), 0xFFFF_F000);
    for (bit, err) in [(12, false), (13, true), (18, true), (19, false), (22, false), (23, true), (30, true)] {
        g.write_ctrl(31, 1 << bit);
        assert_eq!(g.read_ctrl(31) & ERR != 0, err, "bit {bit}");
    }
    g.write_ctrl(31, 0xFFF);
    assert_eq!(g.read_ctrl(31), 0);
}

#[test]
fn sxy_fifo() {
    let mut g = Gte::new();
    for i in 1..=4u32 {
        g.write_data(15, i);
    }
    assert_eq!([g.read_data(12), g.read_data(13), g.read_data(14)], [2, 3, 4]);
    assert_eq!(g.read_data(15), 4); // SXYP mirrors SXY2
    g.write_data(14, 9); // direct writes do not push
    assert_eq!([g.read_data(12), g.read_data(13), g.read_data(15)], [2, 3, 9]);
}

#[test]
fn irgb_orgb() {
    let mut g = Gte::new();
    g.write_data(28, 0xFFFF_FFFF);
    assert_eq!([g.read_data(9), g.read_data(10), g.read_data(11)], [0xF80; 3]);
    assert_eq!(g.read_data(28), 0x7FFF);
    assert_eq!(g.read_data(29), 0x7FFF);
    g.write_data(28, 0b00011_00010_00001);
    assert_eq!([g.read_data(9), g.read_data(10), g.read_data(11)], [0x80, 0x100, 0x180]);
    // ORGB saturates IR/0x80 to 0..=0x1F.
    g.write_data(9, 0xFFFF_FF00); // -256
    g.write_data(10, 0x7FFF);
    g.write_data(11, 0x00FF); // 0xFF >> 7 = 1
    assert_eq!(g.read_data(29), 1 << 10 | 0x1F << 5);
    g.write_data(29, 0); // ORGB is read-only
    assert_eq!(g.read_data(29), 1 << 10 | 0x1F << 5);
}

#[test]
fn lzcr_counts() {
    let cases = [
        (0, 32),
        (0xFFFF_FFFF, 32),
        (1, 31),
        (0x8000_0000, 1),
        (0x4000_0000, 1),
        (0xC000_0000, 2),
        (0x00FF_FF00, 8),
        (0xFFF0_0000, 12),
    ];
    let mut g = Gte::new();
    for (lzcs, n) in cases {
        g.write_data(30, lzcs);
        assert_eq!(g.read_data(30), lzcs);
        assert_eq!(g.read_data(31), n, "lzcs={lzcs:#x}");
    }
}

// --- RTPS / RTPT -----------------------------------------------------------------

#[test]
fn rtps_known_vectors() {
    let mut g = camera(1000);
    // z = 1000 = H: no scaling.
    let r = g.rtps(Vec3i16::new(100, 50, 0));
    assert_eq!(r, RtpsResult { sxy: ScreenXY::new(260, 170), sz: 1000, ir0: 0x800, flags: 0 });
    assert_eq!(g.mac(), Vec3i32::new(100, 50, 1000));
    assert_eq!(g.read_data(24), 0x80_0000); // MAC0 = DQB + DQA * 0x10000
    // z = 2000: half size.
    let r = g.rtps(Vec3i16::new(-200, 300, 1000));
    assert_eq!(unr_divide(1000, 2000).0, 0x8000);
    assert_eq!(r, RtpsResult { sxy: ScreenXY::new(60, 270), sz: 2000, ir0: 0xC00, flags: 0 });
    assert_eq!([g.read_data(17), g.read_data(18), g.read_data(19)], [0, 1000, 2000]);
    assert_eq!(g.read_data(13), ScreenXY::new(260, 170).to_u32());
}

#[test]
fn rtps_divide_overflow_and_sx_saturation() {
    let mut g = camera(0);
    let r = g.rtps(Vec3i16::new(0, 0, 10)); // SZ=10, H=1000 >= 20
    assert_eq!(r.sz, 10);
    assert_eq!(r.sxy, ScreenXY::new(160, 120));
    assert_eq!(r.flags, ERR | flag::DIV_OVERFLOW);
    // IR0 = (0x1000000 - 128*0x1FFFF) >> 12 = 0
    assert_eq!(r.ir0, 0);

    let mut g = camera(100);
    let r = g.rtps(Vec3i16::new(1000, -1000, 0));
    assert_eq!(r.sxy, ScreenXY::new(0x3FF, -0x400));
    assert_eq!(r.flags, ERR | flag::DIV_OVERFLOW | flag::SX2_SAT | flag::SY2_SAT);
}

#[test]
fn rtps_negative_z_saturates_sz() {
    let mut g = camera(-100);
    let r = g.rtps(Vec3i16::ZERO);
    assert_eq!(r.sz, 0);
    assert_eq!(g.ir().z, -100);
    assert_eq!(r.flags, ERR | flag::SZ_OTZ_SAT | flag::DIV_OVERFLOW);
}

#[test]
fn rtps_ir0_saturation() {
    let mut g = camera(1000);
    g.set_depth_cue(0x100, 0); // IR0 = H/SZ * 1.0
    g.set_translation(Vec3i32::new(0, 0, 500)); // H/SZ = 2 -> overflow, q=0x1FFFF
    let r = g.rtps(Vec3i16::ZERO);
    assert_eq!(r.ir0, 0x1000);
    assert_eq!(r.flags, ERR | flag::DIV_OVERFLOW | flag::IR0_SAT);
}

#[test]
fn rtps_sf0_ir3_flag_quirk() {
    let mut g = camera(0);
    g.set_projection_distance(0);
    // sf=0: MAC3 = 10*0x1000 = 40960 saturates IR3, but MAC3>>12 = 10 is
    // in range so FLAG.22 stays clear. IR1 has no such exemption.
    g.write_data(0, ScreenXY::new(10, 0).to_u32());
    g.write_data(1, 10);
    g.execute(op::encode(op::RTPS, false, false));
    assert_eq!(g.ir(), Vec3i16::new(0x7FFF, 0, 0x7FFF));
    assert_eq!(g.read_data(19), 10);
    assert_eq!(g.flag(), ERR | flag::IR1_SAT);
    // With sf=1 and MAC3>>12 out of range the flag does fire.
    let mut g = camera(0x9000);
    g.execute(op::encode(op::RTPS, true, false));
    assert_eq!(g.ir().z, 0x7FFF);
    assert_ne!(g.flag() & flag::IR3_SAT, 0);
}

#[test]
fn rtps_lm_clamps_ir() {
    let mut g = camera(1000);
    g.write_data(0, ScreenXY::new(-100, -50).to_u32());
    g.execute(op::encode(op::RTPS, true, true));
    assert_eq!(g.ir(), Vec3i16::new(0, 0, 1000));
    assert_eq!(g.flag(), ERR | flag::IR1_SAT | flag::IR2_SAT);
    // SX uses the clamped IR1.
    assert_eq!(g.read_data(14), ScreenXY::new(160, 120).to_u32());
}

#[test]
fn rtpt_equals_three_rtps() {
    let mut base = camera(800);
    base.set_rotation(&rot_matrix(Vec3i16::new(100, 300, 50)));
    let verts = [Vec3i16::new(-300, 200, 100), Vec3i16::new(400, -100, 0), Vec3i16::new(50, 900, -700)];

    let mut a = base.clone();
    let rt = a.rtpt(verts);

    let mut b = base.clone();
    let mut flags = 0;
    let mut last = RtpsResult::default();
    for v in verts {
        last = b.rtps(v);
        flags |= last.flags;
    }
    assert_eq!(rt.sxy, [12, 13, 14].map(|i| ScreenXY::from_u32(b.read_data(i))));
    assert_eq!(rt.sz, [17, 18, 19].map(|i| b.read_data(i) as u16));
    assert_eq!(rt.ir0, last.ir0);
    assert_eq!(rt.flags, flags);
    for i in [8, 9, 10, 11, 24, 25, 26, 27] {
        assert_eq!(a.read_data(i), b.read_data(i), "reg {i}");
    }
}

#[test]
fn mac_44bit_overflow() {
    let mut g = Gte::new();
    g.set_rotation(&Matrix::new([[0x7FFF, 0, 0], [0; 3], [0; 3]]));
    g.set_translation(Vec3i32::new(i32::MAX, i32::MIN, 0));
    g.write_data(0, 0x7FFF);
    g.execute(mvmva(true, false, 0, 0, 0));
    assert_eq!(g.flag(), ERR | flag::MAC1_POS | flag::IR1_SAT | flag::IR2_SAT);
    // Wrapped at 44 bits: (2^31-1)*2^12 + 0x7FFF^2 - 2^44, >> 12.
    let raw = ((i32::MAX as i64) << 12) + 0x7FFF * 0x7FFF - (1 << 44);
    assert_eq!(g.mac().x, (raw >> 12) as i32);
    assert_eq!(g.ir().x, -0x8000);
}

#[test]
fn nclip_sign() {
    let mut g = Gte::new();
    for p in [(0, 0), (10, 0), (0, 10)] {
        g.write_data(15, ScreenXY::new(p.0, p.1).to_u32());
    }
    assert_eq!(g.nclip(), 100);
    for p in [(0, 0), (0, 10), (10, 0)] {
        g.write_data(15, ScreenXY::new(p.0, p.1).to_u32());
    }
    assert_eq!(g.nclip(), -100);
    // Extreme coordinates overflow MAC0.
    for p in [(-0x8000, -0x8000), (0x7FFF, -0x8000), (0x7FFF, 0x7FFF)] {
        g.write_data(15, ScreenXY::new(p.0, p.1).to_u32());
    }
    g.nclip();
    assert_eq!(g.flag(), ERR | flag::MAC0_POS);
}

#[test]
fn avsz3_avsz4() {
    let mut g = Gte::new();
    for (i, z) in [0, 100, 200, 300].into_iter().enumerate() {
        g.write_data(16 + i, z);
    }
    g.set_z_scale(0x555, 0x400);
    assert_eq!(g.avsz3(), 199); // 0x555*600 = 819000 >> 12
    assert_eq!(g.read_data(24), 819000);
    assert_eq!(g.flag(), 0);
    assert_eq!(g.avsz4(), 150);

    g.set_z_scale(0x7FFF, -1);
    for i in 16..20 {
        g.write_data(i, 0xFFFF);
    }
    assert_eq!(g.avsz3(), 0xFFFF);
    assert_eq!(g.flag(), ERR | flag::MAC0_POS | flag::SZ_OTZ_SAT);
    assert_eq!(g.avsz4(), 0);
    assert_eq!(g.flag(), ERR | flag::SZ_OTZ_SAT);
}

// --- Other commands ---------------------------------------------------------------

#[test]
fn mvmva_far_color_bug() {
    let mut g = Gte::new();
    g.set_rotation(&Matrix::IDENTITY);
    g.write_data(0, ScreenXY::new(1, 2).to_u32());
    g.write_data(1, 3);
    g.set_translation(Vec3i32::new(100, 100, 100));
    g.execute(mvmva(true, false, 0, 0, 0));
    assert_eq!(g.mac(), Vec3i32::new(101, 102, 103));

    // cv=2: FC and the first matrix column are dropped from the result,
    // but still raise flags.
    g.set_far_color(Vec3i32::new(0x8_0000, 0, 0));
    g.execute(mvmva(true, false, 0, 0, 2));
    assert_eq!(g.mac(), Vec3i32::new(0, 2, 3));
    assert_eq!(g.ir(), Vec3i16::new(0, 2, 3));
    assert_eq!(g.flag(), ERR | flag::IR1_SAT);
}

#[test]
fn mvmva_garbage_matrix_and_ir_vector() {
    let mut g = Gte::new();
    g.write_ctrl(1, 7); // R13
    g.write_ctrl(2, 9); // R22
    g.write_data(6, 1); // RGBC.r
    g.write_data(8, 5); // IR0
    g.write_data(28, 0); // IR1-3 = 0
    for r in [9, 10, 11] {
        g.write_data(r, 1);
    }
    g.execute(mvmva(false, false, 3, 3, 3));
    assert_eq!(g.mac(), Vec3i32::new(5, 21, 27)); // -16 + 16 + IR0
    assert_eq!(g.ir(), Vec3i16::new(5, 21, 27));
}

#[test]
fn op_cross_product() {
    let mut g = Gte::new();
    g.set_rotation(&Matrix::IDENTITY);
    g.write_data(9, 0x100);
    g.write_data(10, 0x200);
    g.write_data(11, 0x300);
    g.execute(op::encode(op::OP, true, false));
    assert_eq!(g.mac(), Vec3i32::new(0x100, -0x200, 0x100));
    g.write_data(9, 0x100);
    g.write_data(10, 0x200);
    g.write_data(11, 0x300);
    g.execute(op::encode(op::OP, true, true));
    assert_eq!(g.ir(), Vec3i16::new(0x100, 0, 0x100)); // lm=1 clamps the -0x200 result
    assert_eq!(g.flag(), ERR | flag::IR2_SAT);
}

#[test]
fn sqr_saturation() {
    let mut g = Gte::new();
    g.write_data(9, -3i32 as u32);
    g.write_data(10, 4);
    g.write_data(11, 0x7FFF);
    g.execute(op::encode(op::SQR, false, false));
    assert_eq!(g.mac(), Vec3i32::new(9, 16, 0x3FFF_0001));
    assert_eq!(g.ir(), Vec3i16::new(9, 16, 0x7FFF));
    assert_eq!(g.flag(), flag::IR3_SAT); // bit 22 does not set bit 31
}

#[test]
fn gpf_gpl_and_color_fifo() {
    let mut g = Gte::new();
    g.write_data(6, 0x2C00_0000); // code byte
    g.write_data(8, 0x800);
    g.write_data(9, 0x1000);
    g.write_data(10, 0x2000);
    g.write_data(11, (-0x1000i32) as u32);
    g.execute(op::encode(op::GPF, true, false));
    assert_eq!(g.mac(), Vec3i32::new(0x800, 0x1000, -0x800));
    assert_eq!(Rgbc::from_u32(g.read_data(22)), Rgbc::new(0x80, 0xFF, 0, 0x2C));
    assert_eq!(g.flag(), flag::COLOR_G_SAT | flag::COLOR_B_SAT);

    g.write_data(25, 1);
    g.write_data(26, 2);
    g.write_data(27, 3);
    g.execute(op::encode(op::GPL, true, false));
    assert_eq!(g.mac(), Vec3i32::new(1 + 0x400, 2 + 0x800, 3 - 0x400));
    assert_eq!(g.read_data(21), Rgbc::new(0x80, 0xFF, 0, 0x2C).to_u32()); // FIFO shifted
}

#[test]
fn dpcs_interpolates_to_far_color() {
    let mut g = Gte::new();
    g.write_data(6, Rgbc::new(100, 0, 255, 0).to_u32());
    g.set_far_color(Vec3i32::new(255 << 4, 255 << 4, 0));
    g.write_data(8, 0x800);
    g.execute(op::encode(op::DPCS, true, false));
    assert_eq!(Rgbc::from_u32(g.read_data(22)), Rgbc::new(177, 127, 127, 0));
    // INTPL from IR = rgb<<4 gives the same result.
    let mut h = g.clone();
    for (r, v) in [(9, 100 << 4), (10, 0), (11, 255 << 4)] {
        h.write_data(r, v);
    }
    h.execute(op::encode(op::INTPL, true, false));
    assert_eq!(h.read_data(22), g.read_data(22));
}

/// LLM: one light along +Z; LCM: that light is white; BK = 0.
fn lit() -> Gte {
    let mut g = Gte::new();
    g.set_light_matrix(&Matrix::new([[0, 0, 0x1000], [0; 3], [0; 3]]));
    g.set_color_matrix(&Matrix::new([[0x1000, 0, 0], [0x1000, 0, 0], [0x1000, 0, 0]]));
    g
}

#[test]
fn ncds_lighting_and_depth_cue() {
    let n = Vec3i16::new(0, 0, 0x1000);
    let c = Rgbc::new(100, 150, 200, 0x30);
    let mut g = lit();
    assert_eq!(g.ncds(n, c), c); // IR0 = 0: no fog
    assert_eq!(g.flag(), 0);
    g.write_data(8, 0x800);
    assert_eq!(g.ncds(n, c), Rgbc::new(50, 75, 100, 0x30)); // halfway to FC=black
    g.write_data(8, 0x1000);
    assert_eq!(g.ncds(n, c), Rgbc::new(0, 0, 0, 0x30));
    // Back-facing normal: lm=1 clamps to zero light.
    g.write_data(8, 0);
    assert_eq!(g.ncds(Vec3i16::new(0, 0, -0x1000), c), Rgbc::new(0, 0, 0, 0x30));
    assert_eq!(g.flag(), ERR | flag::IR1_SAT);
    assert_eq!(g.nccs(n, c), c);
}

#[test]
fn ncs_color_saturation() {
    let mut g = lit();
    g.set_back_color(Vec3i32::new(0x2000, -0x1000, 0));
    let out = g.ncs(Vec3i16::ZERO, Rgbc::new(1, 2, 3, 0x20));
    assert_eq!(out, Rgbc::new(255, 0, 0, 0x20));
    assert_eq!(g.flag(), ERR | flag::IR2_SAT | flag::COLOR_R_SAT | flag::COLOR_G_SAT);
}

#[test]
fn triple_color_commands_push_three() {
    let mut g = lit();
    g.write_data(6, Rgbc::new(10, 20, 30, 1).to_u32());
    for (i, z) in [0x1000, 0x800, 0].into_iter().enumerate() {
        g.write_data(i * 2 + 1, z);
    }
    g.execute(op::encode(op::NCCT, true, true));
    let rgb = [20, 21, 22].map(|r| Rgbc::from_u32(g.read_data(r)));
    assert_eq!(rgb, [Rgbc::new(10, 20, 30, 1), Rgbc::new(5, 10, 15, 1), Rgbc::new(0, 0, 0, 1)]);
}

#[test]
fn execute_decoding() {
    let mut g = Gte::new();
    g.write_ctrl(31, 0x7FFF_F000);
    assert_eq!(g.execute(0x3B), None); // unassigned: untouched
    assert_eq!(g.read_ctrl(31), 0xFFFF_F000);
    // Real-world libgte encoding with junk upper bits.
    assert_eq!(g.execute(0x4A18_0001), Some(15));
    assert_eq!(g.execute(0x4A28_0030), Some(23));
}

// --- math ---------------------------------------------------------------------------

#[test]
fn trig_table() {
    assert_eq!([rsin(0), rsin(1024), rsin(2048), rsin(3072)], [0, 4096, 0, -4096]);
    assert_eq!([rcos(0), rcos(1024), rcos(2048)], [4096, 0, -4096]);
    assert_eq!(rsin(512), 2896);
    assert_eq!((1..=6).map(rsin).collect::<Vec<_>>(), [6, 13, 19, 25, 31, 38]);
    assert_eq!(rsin(-1), -6);
    assert_eq!(rsin(4096 + 5), rsin(5));
    for a in 0..4096 {
        assert_eq!(rsin(a), -rsin(-a));
    }
}

#[test]
fn matrix_helpers() {
    assert_eq!(rot_matrix(Vec3i16::ZERO), Matrix::IDENTITY);
    let m = rot_matrix(Vec3i16::new(0, 0, 1024));
    assert_eq!(apply_matrix(&m, Vec3i16::new(100, 0, 0)), Vec3i32::new(0, 100, 0));
    let m = rot_matrix(Vec3i16::new(1024, 0, 0));
    assert_eq!(apply_matrix_sv(&m, Vec3i16::new(0, 100, 0)), Vec3i16::new(0, 0, 100));
    let r = rot_matrix(Vec3i16::new(123, 456, 789));
    assert_eq!(mul_matrix(&Matrix::IDENTITY, &r), r);
    assert_eq!(r.m[0][2] as i32, rsin(456)); // Rx·Ry·Rz puts sin(y) at m[0][2]
    let rx = rot_matrix(Vec3i16::new(123, 0, 0));
    let ry = rot_matrix(Vec3i16::new(0, 456, 0));
    let rz = rot_matrix(Vec3i16::new(0, 0, 789));
    assert_eq!(r, mul_matrix(&rx, &mul_matrix(&ry, &rz)));
    // R · Rᵀ ≈ I.
    let p = mul_matrix(&r, &r.transpose());
    for i in 0..3 {
        for j in 0..3 {
            let want = if i == j { 0x1000 } else { 0 };
            assert!((p.m[i][j] as i32 - want).abs() <= 4);
        }
    }
}

#[test]
fn mul_matrix_matches_gte_mvmva() {
    let a = rot_matrix(Vec3i16::new(300, -200, 1000));
    let b = Matrix::new([[0x7000, -0x7000, 3], [0x1234, 0x7FFF, -5], [-0x8000, 7, 0x4000]]);
    let p = mul_matrix(&a, &b);
    let mut g = Gte::new();
    g.set_rotation(&a);
    for c in 0..3 {
        g.write_data(0, ScreenXY::new(b.m[0][c], b.m[1][c]).to_u32());
        g.write_data(1, b.m[2][c] as u16 as u32);
        g.execute(mvmva(true, false, 0, 0, 3));
        assert_eq!(g.ir(), Vec3i16::new(p.m[0][c], p.m[1][c], p.m[2][c]));
    }
}
