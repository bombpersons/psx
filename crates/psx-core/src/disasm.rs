//! MIPS I (R3000A + COP0/GTE) disassembly in conventional assembler syntax.

const REG: [&str; 32] = [
    "zero", "at", "v0", "v1", "a0", "a1", "a2", "a3", "t0", "t1", "t2", "t3", "t4", "t5", "t6",
    "t7", "s0", "s1", "s2", "s3", "s4", "s5", "s6", "s7", "t8", "t9", "k0", "k1", "gp", "sp", "fp",
    "ra",
];

pub fn reg(r: u32) -> &'static str {
    REG[(r & 31) as usize]
}

/// Disassemble `i` located at `pc` (used for branch targets).
pub fn disasm(i: u32, pc: u32) -> String {
    let op = i >> 26;
    let rs = (i >> 21) & 31;
    let rt = (i >> 16) & 31;
    let rd = (i >> 11) & 31;
    let sh = (i >> 6) & 31;
    let imm = i & 0xFFFF;
    let simm = imm as u16 as i16 as i32;
    let btarget = pc.wrapping_add(4).wrapping_add((simm << 2) as u32);
    let jtarget = (pc.wrapping_add(4) & 0xF000_0000) | ((i & 0x03FF_FFFF) << 2);
    let (r_s, r_t, r_d) = (reg(rs), reg(rt), reg(rd));
    let mem = |name: &str| format!("{name:<7} {r_t}, {simm}({r_s})");
    match op {
        0x00 => {
            let f = i & 0x3F;
            match f {
                0x00 if i == 0 => "nop".into(),
                0x00 => format!("sll     {r_d}, {r_t}, {sh}"),
                0x02 => format!("srl     {r_d}, {r_t}, {sh}"),
                0x03 => format!("sra     {r_d}, {r_t}, {sh}"),
                0x04 => format!("sllv    {r_d}, {r_t}, {r_s}"),
                0x06 => format!("srlv    {r_d}, {r_t}, {r_s}"),
                0x07 => format!("srav    {r_d}, {r_t}, {r_s}"),
                0x08 => format!("jr      {r_s}"),
                0x09 => format!("jalr    {r_d}, {r_s}"),
                0x0C => format!("syscall 0x{:X}", (i >> 6) & 0xFFFFF),
                0x0D => format!("break   0x{:X}", (i >> 6) & 0xFFFFF),
                0x10 => format!("mfhi    {r_d}"),
                0x11 => format!("mthi    {r_s}"),
                0x12 => format!("mflo    {r_d}"),
                0x13 => format!("mtlo    {r_s}"),
                0x18 => format!("mult    {r_s}, {r_t}"),
                0x19 => format!("multu   {r_s}, {r_t}"),
                0x1A => format!("div     {r_s}, {r_t}"),
                0x1B => format!("divu    {r_s}, {r_t}"),
                0x20..=0x27 | 0x2A | 0x2B => {
                    let n = [
                        "add", "addu", "sub", "subu", "and", "or", "xor", "nor", "", "", "slt",
                        "sltu",
                    ][(f - 0x20) as usize];
                    format!("{n:<7} {r_d}, {r_s}, {r_t}")
                }
                _ => format!(".word   0x{i:08X}"),
            }
        }
        0x01 => {
            let link = (rt >> 1) & 0xF == 0x8;
            let n = match (rt & 1 != 0, link) {
                (false, false) => "bltz",
                (true, false) => "bgez",
                (false, true) => "bltzal",
                (true, true) => "bgezal",
            };
            format!("{n:<7} {r_s}, 0x{btarget:08X}")
        }
        0x02 => format!("j       0x{jtarget:08X}"),
        0x03 => format!("jal     0x{jtarget:08X}"),
        0x04 if rs == 0 && rt == 0 => format!("b       0x{btarget:08X}"),
        0x04 => format!("beq     {r_s}, {r_t}, 0x{btarget:08X}"),
        0x05 => format!("bne     {r_s}, {r_t}, 0x{btarget:08X}"),
        0x06 => format!("blez    {r_s}, 0x{btarget:08X}"),
        0x07 => format!("bgtz    {r_s}, 0x{btarget:08X}"),
        0x08 => format!("addi    {r_t}, {r_s}, {simm}"),
        0x09 if rs == 0 => format!("li      {r_t}, {simm}"),
        0x09 => format!("addiu   {r_t}, {r_s}, {simm}"),
        0x0A => format!("slti    {r_t}, {r_s}, {simm}"),
        0x0B => format!("sltiu   {r_t}, {r_s}, {simm}"),
        0x0C => format!("andi    {r_t}, {r_s}, 0x{imm:X}"),
        0x0D => format!("ori     {r_t}, {r_s}, 0x{imm:X}"),
        0x0E => format!("xori    {r_t}, {r_s}, 0x{imm:X}"),
        0x0F => format!("lui     {r_t}, 0x{imm:X}"),
        0x10 => match rs {
            0x00 => format!("mfc0    {r_t}, cop0r{rd}"),
            0x04 => format!("mtc0    {r_t}, cop0r{rd}"),
            0x10 if i & 0x3F == 0x10 => "rfe".into(),
            _ => format!(".word   0x{i:08X}"),
        },
        0x12 if i & (1 << 25) != 0 => format!("cop2    0x{:07X}", i & 0x01FF_FFFF),
        0x12 => match rs {
            0x00 => format!("mfc2    {r_t}, gte_d{rd}"),
            0x02 => format!("cfc2    {r_t}, gte_c{rd}"),
            0x04 => format!("mtc2    {r_t}, gte_d{rd}"),
            0x06 => format!("ctc2    {r_t}, gte_c{rd}"),
            _ => format!(".word   0x{i:08X}"),
        },
        0x20 => mem("lb"),
        0x21 => mem("lh"),
        0x22 => mem("lwl"),
        0x23 => mem("lw"),
        0x24 => mem("lbu"),
        0x25 => mem("lhu"),
        0x26 => mem("lwr"),
        0x28 => mem("sb"),
        0x29 => mem("sh"),
        0x2A => mem("swl"),
        0x2B => mem("sw"),
        0x2E => mem("swr"),
        0x32 => format!("lwc2    gte_d{rt}, {simm}({r_s})"),
        0x3A => format!("swc2    gte_d{rt}, {simm}({r_s})"),
        _ => format!(".word   0x{i:08X}"),
    }
}

#[cfg(test)]
mod tests {
    use super::disasm;

    #[test]
    fn common_forms() {
        assert_eq!(disasm(0, 0), "nop");
        assert_eq!(disasm(0xAC62_4808, 0), "sw      v0, 18440(v1)");
        assert_eq!(disasm(0x3C08_0013, 0), "lui     t0, 0x13");
        assert_eq!(
            disasm(0x1509_FFFD, 0x8001_004C),
            "bne     t0, t1, 0x80010044"
        );
        assert_eq!(disasm(0x0C00_4000, 0x8001_0000), "jal     0x80010000");
        assert_eq!(disasm(0x4200_0010, 0), "rfe");
    }
}
