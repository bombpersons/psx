//! The C API in `harness/driver.cpp`. Mednafen keeps its state in globals, so
//! there is one emulator per process.
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::path::Path;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Frame {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub line_width: i32,
    pub interlaced: i32,
    pub field: i32,
    pub master_cycles: i64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Regs {
    pub gpr: [u32; 32],
    pub hi: u32,
    pub lo: u32,
    pub pc: u32,
    pub sr: u32,
    pub cause: u32,
    pub epc: u32,
    pub badvaddr: u32,
    /// The delayed load in flight: register (32 = none) and value.
    pub load_reg: u32,
    pub load_value: u32,
}

type HookFn = extern "C" fn(pc: u32, breakpoint: c_int, user: *mut c_void);

unsafe extern "C" {
    fn mdh_error() -> *const c_char;
    fn mdh_version() -> *const c_char;
    fn mdh_verbose(on: c_int);
    fn mdh_open(
        base: *const c_char,
        bios: *const c_char,
        media: *const c_char,
        region: *const c_char,
        dualshock: c_int,
    ) -> c_int;
    fn mdh_set_pad(buttons: u16, sticks: *const u8);
    fn mdh_vblanks() -> u64;
    fn mdh_frame(out: *mut Frame, render: c_int) -> c_int;
    fn mdh_pixels(pitch: *mut i32) -> *const u32;
    fn mdh_read(space: *const c_char, address: u32, length: u32, buffer: *mut u8) -> c_int;
    fn mdh_peek(address: u32, bytes: u32) -> u32;
    fn mdh_regs(out: *mut Regs);
    fn mdh_gte(out: *mut u32);
    fn mdh_gpu_busy() -> c_int;
    fn mdh_gpu_readout_field() -> c_int;
    fn mdh_step_hook(cb: Option<HookFn>, user: *mut c_void);
    fn mdh_write_watch(address: u32, length: u32, cb: HookFn, user: *mut c_void);
    fn mdh_save_state(path: *const c_char) -> c_int;
    fn mdh_load_state(path: *const c_char) -> c_int;
    fn mdh_close();
}

pub fn version() -> String {
    // SAFETY: a static NUL-terminated string.
    unsafe { CStr::from_ptr(mdh_version()) }.to_string_lossy().into_owned()
}

fn c_path(p: &Path) -> Result<CString, String> {
    CString::new(p.to_string_lossy().as_bytes()).map_err(|_| format!("{}: path has a NUL byte", p.display()))
}

fn last_error(what: &str) -> String {
    // SAFETY: the driver's error string lives until the next call.
    let e = unsafe { CStr::from_ptr(mdh_error()) }.to_string_lossy().into_owned();
    if e.is_empty() { what.to_string() } else { format!("{what}: {e}") }
}

/// A closure Mednafen's CPU calls back into, boxed twice for a thin pointer.
type Hook = Box<Box<dyn FnMut(u32, bool)>>;

extern "C" fn trampoline(pc: u32, breakpoint: c_int, user: *mut c_void) {
    // SAFETY: `user` is the `Hook` the emulator owns while it is installed.
    let hook = unsafe { &mut *(user as *mut Box<dyn FnMut(u32, bool)>) };
    hook(pc, breakpoint != 0);
}

pub struct Mednafen {
    hook: Option<Hook>,
}

/// Console regions as Mednafen names them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Region {
    Japan,
    America,
    Europe,
}

impl Region {
    /// From the BIOS's version string at 7FF32h ("System ROM Version 4.1
    /// 12/16/97 E"); v1.0 BIOSes have none, and are Japanese.
    pub fn of_bios(bios: &[u8]) -> Region {
        let text = bios.get(0x7FF32..0x7FF32 + 40).unwrap_or_default();
        let end = text.iter().position(|&b| b == 0).unwrap_or(text.len());
        match text.starts_with(b"System ROM Version").then(|| text[..end].last()).flatten() {
            Some(b'A') => Region::America,
            Some(b'E') => Region::Europe,
            _ => Region::Japan,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Region::Japan => "jp",
            Region::America => "na",
            Region::Europe => "eu",
        }
    }
}

impl Mednafen {
    /// `base` holds Mednafen's own files; `media` None boots the BIOS with
    /// the drive closed and empty.
    pub fn open(
        base: &Path,
        bios: &Path,
        media: Option<&Path>,
        region: Region,
        dualshock: bool,
        verbose: bool,
    ) -> Result<Self, String> {
        std::fs::create_dir_all(base).map_err(|e| format!("{}: {e}", base.display()))?;
        let (base, bios) = (c_path(base)?, c_path(bios)?);
        let media = media.map(c_path).transpose()?;
        let region = CString::new(region.name()).unwrap();
        // SAFETY: valid C strings; called once per process.
        unsafe {
            mdh_verbose(verbose as c_int);
            let media = media.as_ref().map_or(std::ptr::null(), |m| m.as_ptr());
            if mdh_open(base.as_ptr(), bios.as_ptr(), media, region.as_ptr(), dualshock as c_int) != 0 {
                return Err(last_error("mednafen can't start"));
            }
        }
        Ok(Self { hook: None })
    }

    /// Buttons in the pad's bit order; sticks are lx, ly, rx, ry (DualShock).
    pub fn set_pad(&mut self, buttons: u16, sticks: [u8; 4]) {
        // SAFETY: the emulator is open; `sticks` holds 4 bytes.
        unsafe { mdh_set_pad(buttons, sticks.as_ptr()) }
    }

    /// Runs to the next vblank; `render` draws the picture for [`Mednafen::screen`].
    pub fn frame(&mut self, render: bool) -> Frame {
        let mut f = Frame::default();
        // SAFETY: the emulator is open; `f` matches `struct mdh_frame`.
        unsafe { mdh_frame(&mut f, render as c_int) };
        f
    }

    /// Vblanks since power-on.
    pub fn vblanks(&self) -> u64 {
        // SAFETY: a plain read of a counter.
        unsafe { mdh_vblanks() }
    }

    pub fn read(&self, space: &str, address: u32, length: u32) -> Result<Vec<u8>, String> {
        read(space, address, length)
    }

    /// Calls `f(pc, false)` before every instruction until [`Mednafen::unhook`].
    pub fn step_hook(&mut self, f: impl FnMut(u32, bool) + 'static) {
        let mut hook: Hook = Box::new(Box::new(f));
        let user = &mut *hook as *mut Box<dyn FnMut(u32, bool)> as *mut c_void;
        // SAFETY: the hook lives in `self` until unhooked or dropped.
        unsafe { mdh_step_hook(Some(trampoline), user) };
        self.hook = Some(hook);
    }

    /// Calls `f(pc, true)` before CPU stores that may touch the range.
    pub fn write_watches(&mut self, ranges: &[(u32, u32)], f: impl FnMut(u32, bool) + 'static) {
        let mut hook: Hook = Box::new(Box::new(f));
        let user = &mut *hook as *mut Box<dyn FnMut(u32, bool)> as *mut c_void;
        for &(address, length) in ranges {
            // SAFETY: as for `step_hook`.
            unsafe { mdh_write_watch(address, length, trampoline, user) };
        }
        self.hook = Some(hook);
    }

    pub fn unhook(&mut self) {
        // SAFETY: removing the hook before freeing it.
        unsafe { mdh_step_hook(None, std::ptr::null_mut()) };
        self.hook = None;
    }

    /// The displayed picture of the last frame, as RGBA.
    pub fn screen(&self, f: &Frame) -> (u32, u32, Vec<u8>) {
        let (w, h) = (f.line_width.max(0) as usize, f.h.max(0) as usize);
        let mut pitch = 0;
        // SAFETY: the surface holds fb_height rows of `pitch` pixels, and the
        // display rectangle lies inside it.
        let pixels = unsafe { mdh_pixels(&mut pitch) };
        let mut rgba = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            let row = unsafe { pixels.add((f.y as usize + y) * pitch as usize + f.x as usize) };
            for x in 0..w {
                let p = unsafe { *row.add(x) }.to_le_bytes();
                rgba.extend_from_slice(&[p[0], p[1], p[2], 0xFF]);
            }
        }
        (w as u32, h as u32, rgba)
    }

    pub fn save_state(&self, path: &Path) -> Result<(), String> {
        let p = c_path(path)?;
        // SAFETY: valid C string.
        if unsafe { mdh_save_state(p.as_ptr()) } != 0 {
            return Err(last_error("can't save the state"));
        }
        Ok(())
    }

    pub fn load_state(&mut self, path: &Path) -> Result<(), String> {
        let p = c_path(path)?;
        // SAFETY: valid C string.
        if unsafe { mdh_load_state(p.as_ptr()) } != 0 {
            return Err(last_error(&format!("can't load {}", path.display())));
        }
        Ok(())
    }
}

/// The CPU registers; inside a hook, before the instruction at `pc` runs.
pub fn regs() -> Regs {
    let mut r = Regs::default();
    // SAFETY: the emulator is open (only called while a `Mednafen` exists).
    unsafe { mdh_regs(&mut r) };
    r
}

/// The GTE's data registers (0-31) then control registers (32-63).
pub fn gte() -> [u32; 64] {
    let mut r = [0; 64];
    // SAFETY: the emulator is open; `r` holds 64 words.
    unsafe { mdh_gte(r.as_mut_ptr()) };
    r
}

/// The GPU has queued commands, owes drawing time, or is between a quad's halves.
pub fn gpu_busy() -> bool {
    // SAFETY: plain reads of the GPU's state.
    unsafe { mdh_gpu_busy() != 0 }
}

/// The interlaced field the GPU reads out for display, latched at vblank.
pub fn gpu_readout_field() -> bool {
    // SAFETY: a plain read of the GPU's state.
    unsafe { mdh_gpu_readout_field() != 0 }
}

/// A side-effect-free read of the CPU's logical address space.
pub fn peek(address: u32, bytes: u32) -> u32 {
    // SAFETY: as for `regs`.
    unsafe { mdh_peek(address, bytes) }
}

/// One of the debugger's address spaces ("cpu", "ram", "spu", "gpu").
pub fn read(space: &str, address: u32, length: u32) -> Result<Vec<u8>, String> {
    let mut buf = vec![0u8; length as usize];
    let name = CString::new(space).unwrap();
    // SAFETY: `buf` holds `length` bytes.
    if unsafe { mdh_read(name.as_ptr(), address, length, buf.as_mut_ptr()) } != 0 {
        return Err(last_error("read"));
    }
    Ok(buf)
}

impl Drop for Mednafen {
    fn drop(&mut self) {
        // SAFETY: the emulator is open, and closed once; the hook goes first.
        unsafe {
            mdh_step_hook(None, std::ptr::null_mut());
            mdh_close();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Region;

    #[test]
    fn region_comes_from_the_bios_version_string() {
        let mut bios = vec![0u8; 512 * 1024];
        assert_eq!(Region::of_bios(&bios), Region::Japan);
        let text = b"System ROM Version 4.1 12/16/97 E\0";
        bios[0x7FF32..0x7FF32 + text.len()].copy_from_slice(text);
        assert_eq!(Region::of_bios(&bios), Region::Europe);
        bios[0x7FF32 + text.len() - 2] = b'A';
        assert_eq!(Region::of_bios(&bios), Region::America);
    }
}
