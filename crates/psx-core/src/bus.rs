//! System bus: main RAM, scratchpad, BIOS ROM, expansion and I/O.
//!
//! Devices are not modelled yet. I/O is a plain register file that remembers
//! writes and returns them on reads, and every I/O access is recorded so a
//! harness can see (and override) what the CPU exchanged with hardware.

pub const RAM_SIZE: usize = 2 * 1024 * 1024;
pub const BIOS_SIZE: usize = 512 * 1024;
pub const SCRATCH_SIZE: usize = 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Width {
    Byte = 1,
    Half = 2,
    Word = 4,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IoAccess {
    pub addr: u32,
    pub width: Width,
    pub write: bool,
    pub value: u32,
}

pub struct Bus {
    pub ram: Vec<u8>,
    pub scratch: [u8; SCRATCH_SIZE],
    pub bios: Vec<u8>,
    /// Physical 0x1F801000..0x1F803000 as a register file.
    pub io: Vec<u8>,
    pub cache_control: u32,
    /// I/O accesses since the last [`Bus::take_io_log`].
    io_log: Vec<IoAccess>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum BusError {
    /// No device decodes this physical address.
    Unmapped(u32),
}

fn physical(addr: u32) -> u32 {
    if addr >= 0xC000_0000 {
        addr
    } else {
        addr & 0x1FFF_FFFF
    }
}

impl Bus {
    pub fn new(bios: Vec<u8>) -> Self {
        let mut b = bios;
        b.resize(BIOS_SIZE, 0);
        Bus {
            ram: vec![0; RAM_SIZE],
            scratch: [0; SCRATCH_SIZE],
            bios: b,
            io: vec![0; 0x2000],
            cache_control: 0,
            io_log: Vec::new(),
        }
    }

    pub fn take_io_log(&mut self) -> Vec<IoAccess> {
        std::mem::take(&mut self.io_log)
    }

    /// True when a virtual address decodes to the I/O region.
    pub fn is_io(addr: u32) -> bool {
        let p = physical(addr);
        (0x1F80_1000..0x1F80_3000).contains(&p)
    }

    pub fn read(&mut self, addr: u32, width: Width) -> Result<u32, BusError> {
        let p = physical(addr);
        let n = width as usize;
        let le = |b: &[u8], o: usize| -> u32 { b[o..o + n].iter().rev().fold(0u32, |acc, &x| (acc << 8) | x as u32) };
        Ok(match p {
            0x0000_0000..=0x007F_FFFF => le(&self.ram, p as usize % RAM_SIZE),
            0x1F00_0000..=0x1F7F_FFFF => u32::MAX >> (32 - 8 * n),
            0x1F80_0000..=0x1F80_03FF if addr >> 29 != 5 => le(&self.scratch, (p - 0x1F80_0000) as usize),
            0x1F80_1000..=0x1F80_2FFF => {
                let v = le(&self.io, (p - 0x1F80_1000) as usize);
                self.io_log.push(IoAccess { addr: p, width, write: false, value: v });
                v
            }
            0x1FC0_0000..=0x1FC7_FFFF => le(&self.bios, (p - 0x1FC0_0000) as usize),
            0xFFFE_0130 => self.cache_control,
            _ => return Err(BusError::Unmapped(p)),
        })
    }

    pub fn write(&mut self, addr: u32, width: Width, value: u32) -> Result<(), BusError> {
        let p = physical(addr);
        let n = width as usize;
        let put = |b: &mut [u8], o: usize| {
            for i in 0..n {
                b[o + i] = (value >> (8 * i)) as u8;
            }
        };
        match p {
            0x0000_0000..=0x007F_FFFF => put(&mut self.ram, p as usize % RAM_SIZE),
            0x1F00_0000..=0x1F7F_FFFF => {}
            0x1F80_0000..=0x1F80_03FF if addr >> 29 != 5 => put(&mut self.scratch, (p - 0x1F80_0000) as usize),
            0x1F80_1000..=0x1F80_2FFF => {
                put(&mut self.io, (p - 0x1F80_1000) as usize);
                self.io_log.push(IoAccess { addr: p, width, write: true, value });
            }
            0x1FC0_0000..=0x1FC7_FFFF => {}
            0xFFFE_0130 => self.cache_control = value,
            _ => return Err(BusError::Unmapped(p)),
        }
        Ok(())
    }

    /// Overwrite an I/O register's stored value (harness injection).
    pub fn poke_io(&mut self, phys: u32, width: Width, value: u32) {
        let o = (phys - 0x1F80_1000) as usize;
        for i in 0..width as usize {
            self.io[o + i] = (value >> (8 * i)) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ram_mirrors_scratch_and_bios() {
        let mut bios = vec![0u8; 16];
        bios[0..4].copy_from_slice(&0x3C08_0013u32.to_le_bytes());
        let mut b = Bus::new(bios);
        b.write(0x8000_0010, Width::Word, 0xDEAD_BEEF).unwrap();
        assert_eq!(b.read(0xA020_0010, Width::Word).unwrap(), 0xDEAD_BEEF, "2 MiB mirror via KSEG1");
        assert_eq!(b.read(0x0000_0012, Width::Half).unwrap(), 0xDEAD);
        assert_eq!(b.read(0xBFC0_0000, Width::Word).unwrap(), 0x3C08_0013);
        b.write(0x1F80_0004, Width::Byte, 0x5A).unwrap();
        assert_eq!(b.read(0x9F80_0004, Width::Byte).unwrap(), 0x5A);
        assert!(b.read(0xBF80_0004, Width::Byte).is_err(), "no scratchpad through KSEG1");
        b.write(0x1F80_1070, Width::Word, 7).unwrap();
        assert_eq!(b.take_io_log().len(), 1);
    }
}
