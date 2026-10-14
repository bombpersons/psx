//! Boot a disc natively for N frames and print every CD DMA transfer as
//! `frame lba offset bytes` (for diffing against `psx loads` on a Redux run).
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (bios, disc, frames) = (&a[1], &a[2], a[3].parse::<u64>().unwrap());
    let mut m = psx_core::Machine::new(std::fs::read(bios).unwrap());
    m.insert_disc(Some(psx_media::Disc::open(disc).unwrap()));
    m.bus.cdrom.log_dma = true;
    let mut seen = 0;
    while m.bus.frames < frames {
        m.step();
        let log = &m.bus.cdrom.dma_log;
        for (lba, off, bytes) in &log[seen..] {
            println!("{} {} {} {}", m.bus.frames, lba, off, bytes);
        }
        seen = log.len();
    }
    for (t, what, d) in &m.bus.cdrom.irq_log {
        if *t > 809_000_000 && *t < 835_000_000 {
            eprintln!("irq t={t} {what} {d:X}");
        }
    }
    for (t, c, p) in &m.bus.cdrom.cmd_log {
        eprintln!("cmd t={t} frame~{} {:02X} {:02X?}", t / (33_868_800 / 50), c, p);
    }
}
