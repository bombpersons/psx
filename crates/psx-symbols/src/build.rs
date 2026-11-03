//! How a game's code was built, read from the code itself.

use serde::Serialize;

/// What the code shows about its compiler and options. Counts are over every
/// word of the code, data included, so small counts can be data.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Build {
    /// gp as the startup code sets it (small data addressed from gp, GCC's -G).
    pub gp: Option<u32>,
    /// Instructions addressing memory from gp.
    pub gp_accesses: usize,
    pub divisions: usize,
    /// `break 7` after a division: GCC's divide-by-zero check.
    pub zero_checks: usize,
    /// `break 6`: GCC's signed overflow check.
    pub overflow_checks: usize,
    /// Stack frames made (`addiu sp,sp,-N`), about one a function.
    pub frames: usize,
    /// Frames that also set a frame pointer (`move fp,sp`).
    pub frame_pointers: usize,
    /// Loads followed by a nop, in percent: unscheduled code fills every load
    /// delay with one.
    pub load_nops_pct: f64,
    /// Instructions writing gp, k0 or k1 outside the startup code, which
    /// compiled C never does: hand-written code, or data that decodes so.
    pub hand_written: Vec<u32>,
}

fn words(code: &[u8]) -> Vec<u32> {
    code.as_chunks::<4>().0.iter().map(|w| u32::from_le_bytes(*w)).collect()
}

/// Reads [`Build`] from a module's code loaded at `base` with entry `entry`.
pub fn detect(code: &[u8], base: u32, entry: u32) -> Build {
    let w = words(code);
    let op = |x: u32| x >> 26;
    let rs = |x: u32| (x >> 21) & 31;
    let rt = |x: u32| (x >> 16) & 31;
    let rd = |x: u32| (x >> 11) & 31;
    let load = |x: u32| matches!(op(x), 0x20..=0x26);
    let store = |x: u32| matches!(op(x), 0x28 | 0x29 | 0x2A | 0x2B | 0x2E);
    let gp = startup_gp(code, base, entry);
    // The startup's own lui/addiu pair is not hand-written.
    let startup = gp.and_then(|_| {
        let start = (entry.wrapping_sub(base) / 4) as usize % w.len().max(1);
        (0..w.len()).map(|i| (start + i) % w.len()).find(|&i| w[i] >> 16 == 0x3C1C)
    });
    let writes = |x: u32| -> Option<u32> {
        match op(x) {
            0 if !matches!(x & 0x3F, 0x08 | 0x09 | 0x0C | 0x0D | 0x18..=0x1B) => Some(rd(x)),
            0x08..=0x0F => Some(rt(x)),
            0x10 if rs(x) == 0 => Some(rt(x)),
            0x20..=0x26 => Some(rt(x)),
            _ => None,
        }
    };
    let mut b = Build { gp, ..Build::default() };
    let mut loads = 0;
    let mut load_nops = 0;
    for (i, &x) in w.iter().enumerate() {
        if (load(x) || store(x) || op(x) == 0x09) && rs(x) == 28 && !(op(x) == 0x09 && rt(x) == 28) {
            b.gp_accesses += 1;
        }
        if op(x) == 0 && matches!(x & 0x3F, 0x1A | 0x1B) {
            b.divisions += 1;
        }
        match x {
            0x0007_000D => b.zero_checks += 1,
            0x0006_000D => b.overflow_checks += 1,
            0x03A0_F021 | 0x03A0_F025 => b.frame_pointers += 1,
            _ => {}
        }
        if x >> 16 == 0x27BD && x & 0x8000 != 0 {
            b.frames += 1;
        }
        if load(x) {
            loads += 1;
            if w.get(i + 1) == Some(&0) {
                load_nops += 1;
            }
        }
        let near_startup = startup.is_some_and(|s| i >= s && i <= s + 8);
        if x != 0 && writes(x).is_some_and(|r| matches!(r, 26..=28)) && !near_startup {
            b.hand_written.push(base.wrapping_add(i as u32 * 4));
        }
    }
    b.load_nops_pct = if loads == 0 { 0.0 } else { (load_nops as f64 * 1000.0 / loads as f64).round() / 10.0 };
    b
}

/// The gp value the startup code loads (`lui gp,HI` then `addiu`/`ori gp,gp,LO`).
/// The search starts at `entry` and wraps, so the entry's own setup wins over
/// any later reload; `None` when no such pair is found.
pub fn startup_gp(code: &[u8], base: u32, entry: u32) -> Option<u32> {
    let words = words(code);
    if words.is_empty() {
        return None;
    }
    let start = (entry.wrapping_sub(base) / 4) as usize % words.len();
    (0..words.len()).map(|i| (start + i) % words.len()).find_map(|i| {
        let w = words[i];
        // lui $gp (rt = 28), then the low half within the next few instructions.
        if w >> 16 != 0x3C1C {
            return None;
        }
        let hi = (w & 0xFFFF) << 16;
        words.iter().skip(i + 1).take(8).find_map(|&n| match n >> 16 {
            0x279C => Some(hi.wrapping_add(n as i16 as i32 as u32)),
            0x379C => Some(hi | (n & 0xFFFF)),
            _ => None,
        })
    })
}

/// Whether the code just before `entry` ends a function: a jump through a
/// register (`jr ra` and switch tails), a plain jump (a tail call) or a
/// `break`, and the slot after it, then any nop padding. False for an entry inside a function.
pub fn follows_function_end(code: &[u8], base: u32, entry: u32) -> bool {
    let word = |at: u32| -> Option<u32> {
        let o = at.checked_sub(base)? as usize;
        code.get(o..o + 4).map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]]))
    };
    let mut at = entry;
    for _ in 0..8 {
        if at <= base {
            return true;
        }
        let slot = at - 4;
        if slot >= base + 4
            && word(slot - 4)
                .is_some_and(|j| j & 0xFC1F_FFFF == 0x0000_0008 || j >> 26 == 2 || j & 0xFC00_003F == 0x0000_000D)
        {
            return true;
        }
        if word(slot) != Some(0) {
            return false;
        }
        at = slot;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(words: &[u32]) -> Vec<u8> {
        words.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    #[test]
    fn startup_gp_reads_lui_and_addiu_or_ori() {
        // Croc's startup: lui gp,0x8007; addiu gp,gp,0x4B38.
        let c = code(&[0, 0x3C1C8007, 0x279C4B38, 0]);
        assert_eq!(startup_gp(&c, 0x8001_0000, 0x8001_0000), Some(0x8007_4B38));
        // A negative low half borrows from the high half.
        let c = code(&[0x3C1C8008, 0x279C8000]);
        assert_eq!(startup_gp(&c, 0x8001_0000, 0x8001_0000), Some(0x8007_8000));
        let c = code(&[0x3C1C8008, 0x00000000, 0x379C8000]);
        assert_eq!(startup_gp(&c, 0x8001_0000, 0x8001_0000), Some(0x8008_8000));
        assert_eq!(startup_gp(&code(&[0x3C1C8008, 0]), 0x8001_0000, 0x8001_0000), None);
    }

    #[test]
    fn a_function_end_is_a_jump_its_delay_slot_and_padding() {
        let jr_ra = 0x03E0_0008;
        let c = code(&[jr_ra, 0x27BD0010, 0x27BDFFE8]);
        assert!(follows_function_end(&c, 0x8001_0000, 0x8001_0008));
        let c = code(&[jr_ra, 0, 0, 0, 0x27BDFFE8]);
        assert!(follows_function_end(&c, 0x8001_0000, 0x8001_0010));
        // A tail call: j target.
        let c = code(&[0x0800_4000, 0, 0x27BDFFE8]);
        assert!(follows_function_end(&c, 0x8001_0000, 0x8001_0008));
        let c = code(&[0x8FA20014, 0, 0xAF820858]);
        assert!(!follows_function_end(&c, 0x8001_0000, 0x8001_0008));
        assert!(follows_function_end(&c, 0x8001_0000, 0x8001_0000));
    }

    #[test]
    fn detect_counts_gcc_habits_and_hand_written_registers() {
        let c = code(&[
            0x3C1C8007, 0x279C4B38, // startup: lui gp / addiu gp
            0x27BDFFE8, 0x03A0F021, // frame and frame pointer
            0x8F828000, 0x00000000, // lw v0,-0x8000(gp); load delay nop
            0x0043001A, 0x0007000D, 0x0006000D, // div, break 7, break 6
            0x401A6800, // mfc0 k0,cause: an exception handler, hand-written
            0x3C1A8001, // lui k0
        ]);
        let b = detect(&c, 0x8001_0000, 0x8001_0000);
        assert_eq!(b.gp, Some(0x8007_4B38));
        assert_eq!((b.gp_accesses, b.divisions, b.zero_checks, b.overflow_checks), (1, 1, 1, 1));
        assert_eq!((b.frames, b.frame_pointers, b.load_nops_pct), (1, 1, 100.0));
        assert_eq!(b.hand_written, vec![0x8001_0024, 0x8001_0028]);
    }

    #[test]
    fn startup_gp_prefers_the_pair_after_the_entry() {
        let c = code(&[0x3C1C8001, 0x279C0010, 0, 0x3C1C8002, 0x279C0020]);
        assert_eq!(startup_gp(&c, 0x8001_0000, 0x8001_000C), Some(0x8002_0020));
        assert_eq!(startup_gp(&c, 0x8001_0000, 0x8001_0000), Some(0x8001_0010));
    }
}
