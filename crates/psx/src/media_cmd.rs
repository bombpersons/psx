//! Static disc and executable commands: `disc`, `ls` rows, `extract`, `exe`, `sector`.

use std::path::Path;

use psx_media::disc::{sha256_bytes, sha256_file};
use psx_media::iso9660::{Entry, FileSystem};
use psx_media::system_cnf::{SystemCnf, DEFAULT_BOOT};
use psx_media::{Disc, MediaError, PsxExe};
use serde_json::{json, Value};

type R = psx_media::Result<Value>;

pub(crate) fn entry_json(e: &Entry) -> Value {
    json!({
        "path": e.path,
        "lba": e.lba,
        "size": e.size,
        "sectors": e.sectors(),
        "dir": e.is_dir,
        "streamed": e.is_streamed(),
        "xa_attributes": e.xa_attributes.map(|a| format!("0x{a:04X}")),
        "recorded": e.recorded,
    })
}

pub(crate) fn exe_json(exe: &PsxExe, bytes: &[u8]) -> Value {
    let h = |v: u32| format!("0x{v:08X}");
    json!({
        "pc": h(exe.pc),
        "gp": h(exe.gp),
        "text": {"addr": h(exe.text_addr), "size": h(exe.text_size), "end": h(exe.text_range().end), "file_offset": "0x800"},
        "data": {"addr": h(exe.data_addr), "size": h(exe.data_size)},
        "bss": {"addr": h(exe.bss_addr), "size": h(exe.bss_size)},
        "initial_sp": exe.initial_sp().map(h),
        "marker": exe.marker,
        "region_hint": exe.region_hint(),
        "file_size": exe.file_size,
        "sha256": sha256_bytes(bytes),
        "payload_sha256": sha256_bytes(exe.payload(bytes)),
        "warnings": exe.warnings,
    })
}

pub(crate) fn disc_summary(image: &Path, hash: bool) -> R {
    let disc = Disc::open(image)?;
    let files: Vec<Value> = disc
        .files
        .iter()
        .map(|f| {
            let mut v = json!({"name": f.name, "bytes": f.bytes});
            if hash {
                v["sha256"] = json!(sha256_file(&f.path).unwrap_or_else(|e| format!("error: {e}")));
            }
            v
        })
        .collect();
    // Hashes of the sectors as read, so a CHD and the CUE/BIN it came from match.
    let track_sha256 = if hash {
        let rows = disc.tracks.iter().map(|t| Ok(json!({"track": t.number, "sha256": disc.track_sha256(t)?})));
        Some(rows.collect::<psx_media::Result<Vec<_>>>()?)
    } else {
        None
    };
    let audio = disc.tracks.iter().filter(|t| !t.mode.is_data()).count();
    let mut out = json!({
        "source": disc.source,
        "media_files": files,
        "tracks": disc.tracks,
        "track_summary": {"total": disc.tracks.len(), "data": disc.tracks.len() - audio, "audio": audio},
        "end_lba": disc.end_lba(),
        "track_sha256": track_sha256,
        // LibCrypt subchannel data: rips lack it; supply the redump .sbi/.lsd beside the image.
        "subchannel": {
            "source": disc.subq_source,
            "overridden_sectors": disc.subq.len(),
            "first_lbas": disc.subq.keys().take(4).collect::<Vec<_>>(),
        },
    });
    let fs = match FileSystem::read(&disc) {
        Ok(fs) => fs,
        Err(e) => {
            out["filesystem_error"] = json!(e.to_string());
            return Ok(out);
        }
    };
    let files_only = fs.entries.iter().filter(|e| !e.is_dir);
    out["volume"] = json!(fs.volume);
    out["filesystem"] = json!({
        "files": files_only.clone().count(),
        "dirs": fs.entries.iter().filter(|e| e.is_dir).count(),
        "streamed_files": files_only.clone().filter(|e| e.is_streamed()).count(),
        "bytes": files_only.map(|e| e.size as u64).sum::<u64>(),
    });
    out["fs_warnings"] = json!(fs.warnings);

    let cnf = fs.find("/SYSTEM.CNF").map(|e| fs.read_file(&disc, e)).transpose()?.map(|b| SystemCnf::parse(&b));
    let boot = cnf.as_ref().and_then(|c| c.boot.clone()).unwrap_or_else(|| DEFAULT_BOOT.to_string());
    out["system_cnf"] = json!(cnf);
    out["boot_path"] = json!(boot);
    match fs.find(&boot) {
        Some(e) => {
            let bytes = fs.read_file(&disc, e)?;
            out["boot_exe"] = match PsxExe::parse(&bytes) {
                Ok(exe) => json!({"entry": entry_json(e), "header": exe_json(&exe, &bytes)}),
                Err(err) => json!({"entry": entry_json(e), "error": err.to_string()}),
            };
        }
        None => out["boot_exe"] = json!({"error": format!("{boot} not found in file system")}),
    }
    Ok(out)
}

pub(crate) fn extract(image: &Path, path: Option<&str>, all: bool, out: &Path) -> R {
    let disc = Disc::open(image)?;
    let fs = FileSystem::read(&disc)?;
    let selected: Vec<&Entry> = match (path, all) {
        (_, true) => fs.entries.iter().filter(|e| !e.is_dir).collect(),
        (Some(p), false) => vec![fs.entry(p)?],
        (None, false) => return Err(MediaError::Malformed("give a path or --all".into())),
    };
    let mut written = Vec::new();
    let mut failed = Vec::new();
    for e in selected {
        let (bytes, raw) = match fs.read_file(&disc, e) {
            Ok(b) => (b, false),
            Err(MediaError::Unsupported(_)) => match fs.read_file_raw(&disc, e) {
                Ok(b) => (b, true),
                Err(err) => {
                    failed.push(json!({"path": e.path, "error": err.to_string()}));
                    continue;
                }
            },
            Err(err) => {
                failed.push(json!({"path": e.path, "error": err.to_string()}));
                continue;
            }
        };
        let rel = e.path.trim_start_matches('/');
        let dest = out.join(if raw { format!("{rel}.raw") } else { rel.to_string() });
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|err| MediaError::io(parent, err))?;
        }
        std::fs::write(&dest, &bytes).map_err(|err| MediaError::io(&dest, err))?;
        written.push(json!({
            "path": e.path, "lba": e.lba, "dest": dest, "bytes": bytes.len(),
            "format": if raw { "raw_2352" } else { "form1_user_data" },
            "sha256": sha256_bytes(&bytes),
        }));
    }
    Ok(json!({"written": written, "failed": failed}))
}

/// A byte window of one file, read sector by sector rather than extracting the
/// whole file, with the image location of its first byte: file offset `O` is
/// user data byte `O % 2048` of the file's `O / 2048`th sector, which in a raw
/// image follows a 16 (Mode 1) or 24 (Mode 2) byte sector header.
pub(crate) fn extract_window(image: &Path, path: &str, offset: u32, length: u32, out: &Path) -> R {
    let disc = Disc::open(image)?;
    let fs = FileSystem::read(&disc)?;
    let e = fs.entry(path)?;
    if e.is_dir {
        return Err(MediaError::Malformed(format!("{} is a directory", e.path)));
    }
    if offset >= e.size {
        return Err(MediaError::Malformed(format!("offset 0x{offset:X} is past the end of {} (0x{:X} bytes)", e.path, e.size)));
    }
    let length = length.min(e.size - offset);
    let first = offset / 2048;
    let last = (offset + length.max(1) - 1) / 2048;
    let mut bytes = Vec::with_capacity(((last - first + 1) * 2048) as usize);
    let mut header = 0;
    for i in first..=last {
        let s = disc.read_data(e.lba + i)?;
        if s.form == psx_media::disc::SectorForm::Form2 {
            return Err(MediaError::Unsupported(format!(
                "{} has a Form 2 sector at LBA {}; file offsets only address Form 1 data, extract without offset for raw sectors",
                e.path, s.lba
            )));
        }
        if i == first {
            header = match s.form {
                psx_media::disc::SectorForm::Mode1 => {
                    if disc.read_raw(s.lba)?.0.len() > 2048 { 16 } else { 0 }
                }
                _ => 24,
            };
        }
        bytes.extend_from_slice(&s.data[..2048]);
    }
    let start = (offset % 2048) as usize;
    let bytes = &bytes[start..start + length as usize];
    let lba = e.lba + first;
    let location = disc.image_offset(lba).map(|(file, sector)| {
        json!({"file": disc.files[file].name, "sector_offset": format!("0x{sector:X}"),
               "offset": format!("0x{:X}", sector + header + start as u64)})
    });
    let rel = e.path.trim_start_matches('/');
    let dest = out.join(format!("{rel}@0x{offset:X}+0x{length:X}"));
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|err| MediaError::io(parent, err))?;
    }
    std::fs::write(&dest, bytes).map_err(|err| MediaError::io(&dest, err))?;
    let mut v = json!({
        "path": e.path, "file_bytes": e.size, "offset": format!("0x{offset:X}"), "bytes": bytes.len(), "dest": dest,
        "sha256": sha256_bytes(bytes),
        "start": {"lba": lba, "track": disc.track_for_lba(lba).map(|t| t.number), "byte_in_sector": start,
                  "sector_header": header, "image": location},
    });
    if bytes.len() <= 0x100 {
        v["hex"] = json!(bytes.iter().map(|b| format!("{b:02x}")).collect::<String>());
    }
    Ok(v)
}

pub(crate) fn exe_cmd(source: &str) -> R {
    let bytes = psx_media::source::read(source)?;
    let exe = PsxExe::parse(&bytes)?;
    Ok(exe_json(&exe, &bytes))
}

pub(crate) fn sectors(image: &Path, lba: u32, count: u32, preview: usize) -> R {
    let disc = Disc::open(image)?;
    let fs = FileSystem::read(&disc).ok();
    let mut out = Vec::new();
    for l in lba..lba.saturating_add(count.min(256)) {
        let track = disc.track_for_lba(l).map(|t| t.number);
        let owner = fs.as_ref().and_then(|fs| fs.owner_of(l)).map(|e| e.path.clone());
        let v = match disc.read_data(l) {
            Ok(s) => json!({
                "lba": l, "track": track, "owner": owner, "form": s.form,
                "subheader": s.subheader.map(|h| json!({"file": h[0], "channel": h[1], "submode": format!("0x{:02X}", h[2]), "coding": format!("0x{:02X}", h[3])})),
                "preview": s.data.iter().take(preview).map(|b| format!("{b:02x}")).collect::<String>(),
            }),
            Err(e) => json!({"lba": l, "track": track, "owner": owner, "error": e.to_string()}),
        };
        out.push(v);
    }
    Ok(json!({"sectors": out}))
}
