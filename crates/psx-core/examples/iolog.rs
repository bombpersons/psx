//! Boot a disc natively and print I/O accesses to chosen registers between two frames.
//! usage: iolog BIOS DISC FROM_FRAME TO_FRAME ADDR[,ADDR...]
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (bios, disc) = (&a[1], &a[2]);
    let (from, to): (u64, u64) = (a[3].parse().unwrap(), a[4].parse().unwrap());
    let addrs: Vec<u32> = a[5].split(',').map(|s| u32::from_str_radix(s.trim_start_matches("0x"), 16).unwrap()).collect();
    let mut m = psx_core::Machine::new(std::fs::read(bios).unwrap());
    m.insert_disc(Some(psx_media::Disc::open(disc).unwrap()));
    while m.bus.frames < from {
        m.step();
    }
    m.bus.log_io = true;
    while m.bus.frames < to {
        let info = m.step();
        for acc in m.bus.take_io_log() {
            if addrs.contains(&(acc.addr & !3)) {
                println!("frame {} pc {:08X} {} {:08X} = {:08X}", m.bus.frames, info.pc, if acc.write { "W" } else { "R" }, acc.addr, acc.value);
            }
        }
    }
}
