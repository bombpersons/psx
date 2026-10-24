//! Video timing: the GPU's clock, scanlines, fields and vblank, ported from
//! Mednafen's `GPU_Update` (`crates/psx-oracle/mednafen/src/psx/gpu.cpp`).
//!
//! The GPU clock runs at 53.693182 MHz on an NTSC console and 53.203425 MHz
//! on a PAL one (the console's, whatever the display mode). A scanline is
//! 200 clocks of horizontal retrace and then the rest of the line: 3405
//! clocks in all in PAL display mode, 3412 or 3413 alternately in NTSC. A
//! field has 314 lines (PAL) or 263 (NTSC); in interlaced modes 313 or 312
//! and 263 or 262 alternately. Vblank starts at the line GP1(07h) ends the
//! display on and ends at the line it starts it on.

/// GPU clocks per system clock, 16.16 (65536 * GPU clock / (44100 * 768)).
const RATIO_NTSC: u64 = 103_896;
const RATIO_PAL: u64 = 102_948;
/// GPU clocks per dot, by GP1(08h) bits 0-1.
const DOT_DIVIDERS: [u32; 4] = [10, 8, 5, 4];
const HRETRACE: u32 = 200;

/// What a span of time produced, for the timers and the interrupt controller.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Events {
    pub dot_clocks: u64,
    /// Horizontal retraces started (timer 1's hblank source).
    pub hretraces: u64,
    /// Vblank started (the vblank interrupt, and a new frame).
    pub vblanks: u64,
}

/// Whether a BIOS is a PAL console's: the region letter ending its version
/// string at 7FF32h ("System ROM Version 4.1 12/16/97 E"), as Mednafen's
/// frontend in psx-oracle picks the console region.
pub fn bios_is_pal(bios: &[u8]) -> bool {
    let text = bios.get(0x7FF32..0x7FF32 + 40).unwrap_or_default();
    let end = text.iter().position(|&b| b == 0).unwrap_or(text.len());
    text.starts_with(b"System ROM Version") && text[..end].last() == Some(&b'E')
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Video {
    /// The console's video standard (from the BIOS), which sets the GPU clock.
    pub pal_console: bool,
    /// GPU clocks owed, 16.16.
    gpu_clock_frac: u64,
    /// GPU clocks to the end of the current line phase.
    line_clocks: u32,
    /// In horizontal retrace (the line's first 200 clocks).
    hretrace: bool,
    dot_clocks: u32,
    pub scanline: u32,
    lines_per_field: u32,
    /// NTSC lines alternate between 3412 and 3413 clocks.
    phase_change: bool,
    /// The interlaced field being scanned out (GPUSTAT bit 13 is its inverse).
    pub field: bool,
    pub in_vblank: bool,
    /// In 480-line interlace, the field whose lines are read out for display:
    /// the GPU doesn't draw them unless drawing to the display is enabled.
    pub readout_field: bool,
    /// Display lines since vblank, and the VRAM line being read out
    /// (GPUSTAT bit 31 is its parity).
    cur_y_offset: u32,
    pub cur_line_readout: u32,
}

impl Video {
    pub fn new(pal_console: bool) -> Self {
        Video {
            pal_console,
            gpu_clock_frac: 0,
            line_clocks: 3412 - HRETRACE,
            hretrace: false,
            dot_clocks: 0,
            scanline: 0,
            lines_per_field: 263,
            phase_change: false,
            field: false,
            in_vblank: true,
            readout_field: false,
            cur_y_offset: 0,
            cur_line_readout: 0,
        }
    }

    /// System clocks per field in display mode `mode`, on average.
    pub fn field_cycles(&self, mode: u32) -> u64 {
        let (lines, line) = match (mode & 0x20 != 0, mode & 0x08 != 0) {
            (true, true) => (3125, 3405 * 10),
            (true, false) => (2625, 34125),
            (false, true) => (3140, 3405 * 10),
            (false, false) => (2630, 34125),
        };
        let ratio = if self.pal_console { RATIO_PAL } else { RATIO_NTSC };
        (lines * line * 65536 / ratio as u128 / 100) as u64
    }

    /// System clocks until the current line phase ends (Mednafen's next GPU event).
    pub fn clocks_to_phase_end(&self) -> u64 {
        let ratio = if self.pal_console { RATIO_PAL } else { RATIO_NTSC };
        (((self.line_clocks as u64) << 16).saturating_sub(self.gpu_clock_frac).div_ceil(ratio)).max(1)
    }

    /// Advance by `cycles` system clocks with the display set up as `d`.
    pub fn tick(&mut self, cycles: u64, d: &psx_gpu::Display) -> Events {
        let mut ev = Events::default();
        self.gpu_clock_frac += cycles * if self.pal_console { RATIO_PAL } else { RATIO_NTSC };
        let mut gpu_clocks = self.gpu_clock_frac >> 16;
        self.gpu_clock_frac &= 0xFFFF;
        let divider = DOT_DIVIDERS[(d.mode & 3) as usize];
        while gpu_clocks > 0 {
            let chunk = gpu_clocks.min(self.line_clocks as u64) as u32;
            gpu_clocks -= chunk as u64;
            self.line_clocks -= chunk;
            self.dot_clocks += chunk;
            ev.dot_clocks += (self.dot_clocks / divider) as u64;
            self.dot_clocks %= divider;
            if self.line_clocks == 0 {
                self.end_phase(d, &mut ev);
            }
        }
        ev
    }

    fn end_phase(&mut self, d: &psx_gpu::Display, ev: &mut Events) {
        self.hretrace = !self.hretrace;
        if self.hretrace {
            self.line_clocks = HRETRACE;
            ev.hretraces += 1;
            return;
        }
        let pal_mode = d.mode & 0x08 != 0;
        let interlaced = d.mode & 0x20 != 0;
        self.line_clocks = if pal_mode { 3405 } else { 3412 + self.phase_change as u32 } - HRETRACE;
        self.scanline = (self.scanline + 1) % self.lines_per_field;
        self.phase_change = !self.phase_change;
        if self.scanline == self.lines_per_field - 1 {
            self.field = interlaced && !self.field;
        }
        if self.scanline == 0 {
            self.lines_per_field = match (interlaced, pal_mode) {
                (true, true) => 313 - self.field as u32,
                (true, false) => 263 - self.field as u32,
                (false, true) => 314,
                (false, false) => 263,
            };
            if !interlaced {
                self.field = false;
            }
        }
        let (vert_start, vert_end) = (d.v_range.0 as u32, d.v_range.1 as u32);
        if self.scanline == vert_end && !self.in_vblank {
            self.in_vblank = true;
            ev.vblanks += 1;
            self.cur_y_offset = 0;
            self.readout_field = d.mode & 0x24 == 0x24 && !self.field;
        }
        if self.scanline == vert_start && self.in_vblank {
            self.in_vblank = false;
        }
        let y_start = d.start.1 as u32;
        self.cur_line_readout = if d.mode & 0x24 == 0x24 {
            y_start + (self.cur_y_offset << 1) + if self.in_vblank { 0 } else { self.readout_field as u32 }
        } else {
            y_start + self.cur_y_offset
        } & 0x1FF;
        if !self.in_vblank {
            self.cur_y_offset = (self.cur_y_offset + 1) & 0x1FF;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn display(mode: u32) -> psx_gpu::Display {
        psx_gpu::Display { mode, ..Default::default() }
    }

    /// System clocks from one vblank to the next, after the first (the field
    /// length starts NTSC's and changes at scanline 0).
    fn frame_clocks(pal_console: bool, mode: u32) -> Vec<u64> {
        let (mut v, d) = (Video::new(pal_console), display(mode));
        let mut starts = Vec::new();
        for t in 0..4_000_000u64 {
            if v.tick(1, &d).vblanks > 0 {
                starts.push(t);
            }
        }
        starts.windows(2).skip(1).map(|w| w[1] - w[0]).collect()
    }

    #[test]
    fn frames_follow_the_gpu_clock() {
        // NTSC: 263 lines of 3412.5 GPU clocks at 103896/65536 per system clock.
        for f in frame_clocks(false, 0) {
            assert!((566_120..=566_122).contains(&f), "ntsc frame {f}");
        }
        // PAL console in PAL mode: 314 lines of 3405 at 102948/65536 (49.76 Hz).
        for f in frame_clocks(true, 0x08) {
            assert!((680_626..=680_627).contains(&f), "pal frame {f}");
        }
    }

    #[test]
    fn interlaced_fields_alternate_in_length_and_parity() {
        let (mut v, d) = (Video::new(false), display(0x24));
        let mut fields = Vec::new();
        let mut last = 0;
        for t in 0..3_000_000u64 {
            if v.tick(1, &d).vblanks > 0 {
                fields.push((t - last, v.readout_field));
                last = t;
            }
        }
        let fields = &fields[2..];
        assert!(fields.windows(2).all(|w| w[0].1 != w[1].1), "readout field alternates: {fields:?}");
        let lens: Vec<u64> = fields.iter().map(|f| f.0).collect();
        assert!(lens.iter().max().unwrap() - lens.iter().min().unwrap() > 2000, "262/263-line fields: {lens:?}");
    }
}
