//! Boot a disc natively and print I/O accesses to chosen registers between two frames.
//! usage: iolog BIOS DISC FROM_FRAME TO_FRAME ADDR[,ADDR...] [REPLAY.json]
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (bios, disc) = (&a[1], &a[2]);
    let (from, to): (u64, u64) = (a[3].parse().unwrap(), a[4].parse().unwrap());
    let addrs: Vec<u32> =
        a[5].split(',').map(|s| u32::from_str_radix(s.trim_start_matches("0x"), 16).unwrap()).collect();
    let mut m = psx_core::Machine::new(std::fs::read(bios).unwrap());
    m.insert_disc(Some(psx_media::Disc::open(disc).unwrap()));
    // Optional replay: [{"frame": n, "buttons": ["CROSS"]}]
    let mut events: Vec<(u64, u16)> = Vec::new();
    if let Some(path) = a.get(6) {
        let text = std::fs::read_to_string(path).unwrap();
        for e in text.split('{').skip(1) {
            let frame: u64 = e
                .split("\"frame\":")
                .nth(1)
                .unwrap()
                .trim_start()
                .split(|c: char| !c.is_ascii_digit())
                .next()
                .unwrap()
                .parse()
                .unwrap();
            let mut bits = 0;
            for name in e.split('"').skip(1).step_by(2) {
                if let Some(b) = psx_core::sio0::button_from_name(name) {
                    bits |= b;
                }
            }
            events.push((frame, bits));
        }
    }
    let mut next = 0;
    let mut apply = |m: &mut psx_core::Machine| {
        while next < events.len() && events[next].0 <= m.bus.frames {
            m.bus.sio0.buttons = events[next].1;
            next += 1;
        }
    };
    while m.bus.frames < from {
        apply(&mut m);
        m.step();
    }
    m.bus.log_io = true;
    while m.bus.frames < to {
        apply(&mut m);
        let info = m.step();
        for acc in m.bus.take_io_log() {
            if addrs.contains(&(acc.addr & !3)) {
                println!(
                    "frame {} pc {:08X} {} {:08X} = {:08X}",
                    m.bus.frames,
                    info.pc,
                    if acc.write { "W" } else { "R" },
                    acc.addr,
                    acc.value
                );
            }
        }
    }
}
