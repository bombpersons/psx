//! Checks against locally supplied commercial discs in `media/` (not committed).
//! Run with `cargo test -p psx-media -- --ignored`.

use std::path::PathBuf;

use psx_media::iso9660::FileSystem;
use psx_media::system_cnf::SystemCnf;
use psx_media::{Disc, PsxExe};

fn media(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../media").join(name)
}

struct Expect {
    cue: &'static str,
    tracks: usize,
    boot: &'static str,
    exe_sha256: &'static str,
    pc: u32,
    text_addr: u32,
}

fn check(e: Expect) {
    let disc = Disc::open(media(e.cue)).expect("open disc (is media/ populated?)");
    assert_eq!(disc.tracks.len(), e.tracks);
    let fs = FileSystem::read(&disc).unwrap();
    assert!(fs.warnings.is_empty(), "{:?}", fs.warnings);
    let cnf = SystemCnf::parse(&fs.read_file(&disc, fs.find("/SYSTEM.CNF").unwrap()).unwrap());
    let boot = cnf.boot.unwrap();
    assert_eq!(boot, e.boot);
    let bytes = fs.read_file(&disc, fs.find(&boot).unwrap()).unwrap();
    assert_eq!(psx_media::disc::sha256_bytes(&bytes), e.exe_sha256);
    let exe = PsxExe::parse(&bytes).unwrap();
    assert_eq!((exe.pc, exe.text_addr), (e.pc, e.text_addr));
    assert!(exe.warnings.is_empty(), "{:?}", exe.warnings);
}

#[test]
#[ignore = "needs local media/Croc - Legend of the Gobbos.cue"]
fn croc_pal() {
    check(Expect {
        cue: "Croc - Legend of the Gobbos.cue",
        tracks: 1,
        boot: "cdrom:\\SLES_005.93;1",
        exe_sha256: "2be7a171a6338319d7fcc84bc759c4ba19ef89253f89fe79f51bd35210ee8b09",
        pc: 0x8010_6994,
        text_addr: 0x8010_0000,
    });
}

#[test]
#[ignore = "needs local media/Tomb Raider.cue"]
fn tomb_raider_pal() {
    let disc = Disc::open(media("Tomb Raider.cue")).unwrap();
    // Track 2 has a 2-second INDEX 00 pregap stored in the image.
    assert_eq!(disc.tracks[1].start_lba - disc.tracks[1].first_lba, 150);
    assert!(disc.read_data(disc.tracks[1].start_lba).is_err(), "audio sector read as data");
    check(Expect {
        cue: "Tomb Raider.cue",
        tracks: 57,
        boot: "cdrom:sles_004.86;1",
        exe_sha256: "29d311f06b305fd4ab39bc50e62feb6a7f566b99538eec1d721f1fd309a46d47",
        pc: 0x8006_046C,
        text_addr: 0x8001_0000,
    });
}
