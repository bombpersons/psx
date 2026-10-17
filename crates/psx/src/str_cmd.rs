//! `psx str`: decode an STR movie offline into PNG frames, a WAV of its
//! interleaved XA audio and a JSON summary.
//!
//! Sector demux and the BS bitstream → MDEC run-length stage live in
//! `psx_media::str_video` (what games do in software). The run-length data is
//! then decoded by the native core's hardware MDEC model (`psx_core::mdec`),
//! loaded with the standard libpress quant and IDCT tables, exactly as a game
//! feeds it, so frames match what the emulator shows. Audio goes through
//! `psx xa` unchanged.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use psx_core::mdec::Mdec;
use psx_media::iso9660::FileSystem;
use psx_media::str_video::{bs_offset, decode_bs, BsError, BsHeader, Frame, FrameAssembler, StrHeader, STR_HEADER_LEN};
use psx_media::{Disc, MediaError};
use serde_json::{json, Value};

/// Zig-zag position → raster index (the order MDEC quant tables are stored in).
const ZAGZIG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14, 21,
    28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61,
    54, 47, 55, 62, 63,
];

/// MPEG-1 default intra matrix, raster order; libpress uploads it zig-zagged
/// (02h,10h,10h,13h,...) for both luma and chroma.
const DEFAULT_QUANT: [u8; 64] = [
    2, 16, 19, 22, 26, 27, 29, 34, 16, 16, 22, 24, 27, 29, 34, 37, 19, 22, 26, 27, 29, 34, 34, 38, 22, 22, 26, 27, 29,
    34, 37, 40, 22, 26, 27, 29, 32, 35, 40, 48, 26, 27, 29, 32, 35, 40, 48, 58, 26, 27, 29, 34, 38, 46, 56, 69, 27,
    29, 35, 38, 46, 56, 69, 83,
];

/// Standard MDEC(3) IDCT scale table (psx-spx), row-major.
const SCALE_TABLE: [u16; 64] = [
    0x5A82, 0x5A82, 0x5A82, 0x5A82, 0x5A82, 0x5A82, 0x5A82, 0x5A82, 0x7D8A, 0x6A6D, 0x471C, 0x18F8, 0xE707, 0xB8E3,
    0x9592, 0x8275, 0x7641, 0x30FB, 0xCF04, 0x89BE, 0x89BE, 0xCF04, 0x30FB, 0x7641, 0x6A6D, 0xE707, 0x8275, 0xB8E3,
    0x471C, 0x7D8A, 0x18F8, 0x9592, 0x5A82, 0xA57D, 0xA57D, 0x5A82, 0x5A82, 0xA57D, 0xA57D, 0x5A82, 0x471C, 0x8275,
    0x18F8, 0x6A6D, 0x9592, 0xE707, 0x7D8A, 0xB8E3, 0x30FB, 0x89BE, 0x7641, 0xCF04, 0xCF04, 0x7641, 0x89BE, 0x30FB,
    0x18F8, 0xB8E3, 0x6A6D, 0x8275, 0x7D8A, 0x9592, 0x471C, 0xE707,
];

/// The core's MDEC loaded as libpress `DecDCTReset` leaves it.
fn standard_mdec() -> Mdec {
    let mut m = Mdec::default();
    m.write_word(3 << 29);
    for pair in SCALE_TABLE.chunks(2) {
        m.write_word(pair[0] as u32 | (pair[1] as u32) << 16);
    }
    let zz: Vec<u8> = ZAGZIG.iter().map(|&i| DEFAULT_QUANT[i]).collect();
    m.write_word((2 << 29) | 1);
    for _ in 0..2 {
        for q in zz.chunks(4) {
            m.write_word(u32::from_le_bytes([q[0], q[1], q[2], q[3]]));
        }
    }
    m
}

/// Run one macroblock's halfwords through MDEC(1) at 24 bpp; 16×16 RGB.
fn decode_macroblock(m: &mut Mdec, halfwords: &[u16]) -> Vec<u8> {
    let mut words: Vec<u32> = halfwords.chunks(2).map(|c| c[0] as u32 | (*c.get(1).unwrap_or(&0xFE00) as u32) << 16).collect();
    if words.is_empty() {
        words.push(0xFE00_FE00);
    }
    m.write_word((1 << 29) | (2 << 27) | words.len() as u32);
    for w in words {
        m.write_word(w);
    }
    let n = m.out_available();
    let bytes: Vec<u8> = (0..n).flat_map(|_| m.dma_out_word().to_le_bytes()).collect();
    let mut rgb = vec![0u8; 16 * 16 * 3];
    let k = bytes.len().min(rgb.len());
    rgb[..k].copy_from_slice(&bytes[..k]);
    rgb
}

/// Decode a BS frame to a `width × height` RGB image (macroblocks column-major).
pub fn decode_frame(m: &mut Mdec, data: &[u8], width: u16, height: u16) -> Result<(BsHeader, Vec<u8>, bool), BsError> {
    let (header, rle) = decode_bs(data, width, height)?;
    let (w, h) = (width as usize, height as usize);
    let mb_h = h.div_ceil(16);
    let mut img = vec![0u8; w * h * 3];
    for i in 0..rle.macroblocks.len() {
        let mb = decode_macroblock(m, rle.macroblock(i));
        let (x0, y0) = ((i / mb_h) * 16, (i % mb_h) * 16);
        for y in 0..16 {
            for x in 0..16 {
                let (px, py) = (x0 + x, y0 + y);
                if px < w && py < h {
                    let s = (y * 16 + x) * 3;
                    let d = (py * w + px) * 3;
                    img[d..d + 3].copy_from_slice(&mb[s..s + 3]);
                }
            }
        }
    }
    Ok((header, img, rle.end_code))
}

/// `A..B`, `A..`, `..B` or `N` (1-based, inclusive).
fn parse_range(s: &str) -> Result<(usize, usize), MediaError> {
    let bad = || MediaError::Malformed(format!("bad frame range {s:?}, expected A..B"));
    let num = |t: &str, d: usize| if t.is_empty() { Ok(d) } else { t.parse::<usize>().map_err(|_| bad()) };
    let (a, b) = match s.split_once("..") {
        Some((a, b)) => (num(a, 1)?, num(b.trim_start_matches('='), usize::MAX)?),
        None => {
            let n = num(s, 1)?;
            (n, n)
        }
    };
    if a == 0 || b < a {
        return Err(bad());
    }
    Ok((a, b))
}

pub struct Args<'a> {
    pub image: &'a Path,
    pub path: &'a str,
    pub out: &'a Path,
    pub frames: Option<&'a str>,
    pub channel: Option<u8>,
    pub no_audio: bool,
    pub y4m: bool,
    pub frame_parser: Option<&'a crate::parser::Spec>,
    pub cancel: Option<&'a tokio_util::sync::CancellationToken>,
}

/// Split `image.cue::/PATH` (or take a separate path argument).
pub fn split_source(source: &str, path: Option<&str>) -> Result<(PathBuf, String), MediaError> {
    match (source.split_once("::"), path) {
        (Some((img, p)), None) => Ok((PathBuf::from(img), p.to_string())),
        (None, Some(p)) => Ok((PathBuf::from(source), p.to_string())),
        (Some(_), Some(_)) => Err(MediaError::Malformed("give the path either as IMAGE::/PATH or as a second argument".into())),
        (None, None) => Err(MediaError::Malformed("missing ISO path: use IMAGE::/PATH or IMAGE /PATH".into())),
    }
}

#[derive(Default)]
struct Scan {
    video: u32,
    audio: u32,
    other_str: BTreeMap<String, u32>,
    other: u32,
    frames: Vec<Frame>,
    /// XA channel `(file, channel)` -> (coding byte of the first sector, sector LBAs).
    channels: BTreeMap<(u8, u8), (u8, Vec<u32>)>,
}

fn scan(disc: &Disc, lba: u32, sectors: u32) -> psx_media::Result<Scan> {
    let mut s = Scan::default();
    let mut asm = FrameAssembler::default();
    for l in lba..lba + sectors {
        let d = disc.read_data(l)?;
        if let Some(sh) = d.subheader.filter(|sh| sh[2] & 0x04 != 0) {
            s.audio += 1;
            s.channels.entry((sh[0], sh[1])).or_insert((sh[3], Vec::new())).1.push(l);
            continue;
        }
        match StrHeader::parse(&d.data) {
            Some(h) if h.is_video() => {
                s.video += 1;
                // Form 1 chunks carry 7E0h bytes; Form 2 (rare) the rest of the 2324.
                let end = if d.data.len() >= 2048 + 276 { d.data.len() } else { STR_HEADER_LEN + 0x7E0 };
                s.frames.extend(asm.push(l, &h, &d.data[..end.min(d.data.len())]));
            }
            Some(h) => *s.other_str.entry(format!("0x{:04X}", h.kind)).or_default() += 1,
            None => s.other += 1,
        }
    }
    s.frames.extend(asm.finish());
    Ok(s)
}

pub fn run(a: Args) -> psx_media::Result<Value> {
    let disc = Disc::open(a.image)?;
    let fs = FileSystem::read(&disc)?;
    let entry = fs.find(a.path).ok_or_else(|| MediaError::NotFound(a.path.to_string()))?.clone();
    let scan = scan(&disc, entry.lba, entry.sectors())?;
    let mut result = json!({
        "path": entry.path, "lba": entry.lba, "sectors": entry.sectors(),
        "sector_kinds": {"video": scan.video, "xa_audio": scan.audio, "other_str_types": scan.other_str, "other": scan.other},
    });
    if scan.frames.is_empty() {
        let why = if scan.other_str.is_empty() {
            "no STR video sectors (ID 0160h, type 8001h): not a standard STR stream"
        } else {
            "STR headers present but none with the MDEC type 8001h (custom or polygon stream)"
        };
        return Err(MediaError::Unsupported(format!("{}: {why}; sector kinds {}", entry.path, result["sector_kinds"])));
    }
    // Probe the BS variant of the first frame before writing anything.
    let variants: BTreeMap<String, usize> = scan.frames.iter().fold(BTreeMap::new(), |mut m, f| {
        *m.entry(BsHeader::parse(&f.data[bs_offset(&f.data)..]).map_or("too_short".into(), |h| h.variant())).or_default() += 1;
        m
    });
    if a.frame_parser.is_none() && !variants.keys().any(|v| matches!(v.as_str(), "v1" | "v2" | "v3")) {
        return Err(MediaError::Unsupported(format!(
            "{}: STR video sectors, but no BS v1/v2/v3 bitstreams (variants seen {variants:?}{}); polygon streams, iki, v0, EA and encrypted formats are not decoded",
            entry.path,
            if scan.frames.iter().all(|f| f.width == 0 && f.height == 0) { ", 0x0 dimensions: likely a polygon stream" } else { "" }
        )));
    }

    // Frame rate: frames over the sectors they span. With XA audio, the audio
    // duration fixes the drive speed; otherwise assume 2x (150 sectors/s).
    let span = scan.frames.last().map_or(0, |f| f.last_lba + 1) - scan.frames.first().map_or(0, |f| f.first_lba);
    let frames_total = scan.frames.len();
    let spf = span as f64 / frames_total as f64;
    let mut fps = json!({"sectors_per_frame": round2(spf), "drive_sectors_per_second": 150, "speed_source": "assumed 2x", "fps": round2(150.0 / spf)});
    // Default channel: the one with most sectors (some movies carry a short silent channel).
    let channel = a.channel.or_else(|| scan.channels.iter().max_by_key(|(_, c)| c.1.len()).map(|(k, _)| k.1));
    if let Some((coding, lbas)) = scan.channels.iter().find(|(k, _)| Some(k.1) == channel).map(|(_, c)| c) {
        if lbas.len() >= 2 {
            // The drive must deliver one audio sector per (samples per sector / rate)
            // seconds; the usual interleave (median distance between this channel's
            // audio sectors; streams can have audio-free stretches) gives the speed.
            let c = psx_core::xa::Coding::from_byte(*coding);
            let frames_per_sector = if c.bits == 4 { 4032 } else { 2016 } / if c.stereo { 2 } else { 1 };
            let needed = c.rate as f64 / frames_per_sector as f64;
            let mut gaps: Vec<u32> = lbas.windows(2).map(|w| w[1] - w[0]).collect();
            gaps.sort_unstable();
            let raw = needed * gaps[gaps.len() / 2] as f64;
            // Snap to 1x/2x when within 3% (irregular interleave makes the raw value approximate).
            let speed = [75.0, 150.0].into_iter().find(|s| (raw - s).abs() / s < 0.03).unwrap_or(raw);
            fps["drive_sectors_per_second"] = json!(round2(speed));
            fps["drive_sectors_per_second_raw"] = json!(round2(raw));
            fps["speed_source"] = json!(format!("XA channel {} interleave 1/{}", channel.unwrap_or(0), gaps[gaps.len() / 2]));
            fps["fps"] = json!(round2(speed / spf));
        }
    }
    let fps_value = fps["fps"].as_f64().unwrap_or(15.0);

    std::fs::create_dir_all(a.out).map_err(|e| MediaError::io(a.out, e))?;
    let (first, last) = a.frames.map(parse_range).transpose()?.unwrap_or((1, usize::MAX));
    // Frames kept for the contact sheet: up to 24 evenly spaced over the range.
    let in_range = last.min(frames_total).saturating_sub(first - 1);
    let sheet_count = in_range.min(24);
    let sheet: std::collections::BTreeSet<usize> = (0..sheet_count).map(|i| first + i * in_range / sheet_count).collect();
    let mut mdec = standard_mdec();
    let mut dims: BTreeMap<String, usize> = BTreeMap::new();
    let mut quants = (u16::MAX, 0u16);
    let mut written = Vec::new();
    let mut errors: BTreeMap<String, usize> = BTreeMap::new();
    let mut error_samples = Vec::new();
    let mut incomplete = 0usize;
    let mut no_end_code = 0usize;
    let mut per_frame = Vec::new();
    let mut y4m = None;
    let mut thumbs: Vec<(usize, u16, u16, Vec<u8>)> = Vec::new();
    for (i, f) in scan.frames.iter().enumerate() {
        let index = i + 1;
        *dims.entry(format!("{}x{}", f.width, f.height)).or_default() += 1;
        if !f.missing.is_empty() {
            incomplete += 1;
        }
        if index < first || index > last {
            continue;
        }
        let mut rec = json!({"index": index, "str_frame": f.number, "lba": f.first_lba, "chunks": f.chunks, "missing_chunks": f.missing});
        let normalized;
        let data = if let Some(spec) = a.frame_parser {
            let output = crate::parser::run(
                spec,
                &f.data,
                "video",
                &[
                    ("PSX_SOURCE_FILE", format!("{}::{}", a.image.display(), a.path)),
                    ("PSX_FRAME_INDEX", index.to_string()),
                    ("PSX_STR_FRAME", f.number.to_string()),
                    ("PSX_WIDTH", f.width.to_string()),
                    ("PSX_HEIGHT", f.height.to_string()),
                ],
                a.cancel,
            )?;
            normalized = crate::parser::document::<crate::parser::PayloadDocument>(&output.bytes)?.decode()?;
            let name = format!("frame_{index:05}.bs");
            let path = a.out.join(&name);
            std::fs::write(&path, &normalized).map_err(|e| MediaError::io(&path, e))?;
            rec["parser"] = output.provenance;
            rec["normalized_bs"] = json!(name);
            normalized.as_slice()
        } else {
            &f.data[bs_offset(&f.data)..]
        };
        match decode_frame(&mut mdec, data, f.width, f.height) {
            Ok((h, rgb, end)) => {
                quants = (quants.0.min(h.quant), quants.1.max(h.quant));
                if !end {
                    no_end_code += 1;
                }
                let name = format!("frame_{index:05}.png");
                let p = a.out.join(&name);
                crate::run::write_png(&p, f.width as u32, f.height as u32, &rgb, png::ColorType::Rgb).map_err(|e| MediaError::io(&p, e))?;
                rec["png"] = json!(name);
                rec["bs"] = json!({"version": h.version, "quant": h.quant, "end_code": end});
                if a.y4m {
                    write_y4m(&mut y4m, a.out, f.width, f.height, fps_value, &rgb)?;
                }
                if sheet.contains(&index) {
                    thumbs.push((index, f.width, f.height, rgb));
                }
                written.push(index);
            }
            Err(e) => {
                *errors.entry(match &e {
                    BsError::Unsupported(v) => format!("unsupported_{v}"),
                    BsError::Truncated { .. } => "truncated".into(),
                    BsError::BadCode { .. } => "bad_code".into(),
                    BsError::BlockOverflow { .. } => "block_overflow".into(),
                    BsError::TooShort => "too_short".into(),
                }).or_default() += 1;
                if error_samples.len() < 10 {
                    error_samples.push(json!({"index": index, "str_frame": f.number, "lba": f.first_lba, "error": e.to_string()}));
                }
                rec["error"] = json!(e.to_string());
            }
        }
        per_frame.push(rec);
    }
    if let Some((mut file, _)) = y4m.take() {
        file.flush().map_err(|e| MediaError::io(a.out.join("movie.y4m"), e))?;
    }

    let mut audio = Value::Null;
    if scan.audio > 0 {
        let listing = crate::xa_cmd::run(a.image, a.path, None, None, None)?;
        audio = json!({"channels": listing["channels"]});
        if let (false, Some(ch)) = (a.no_audio, channel) {
            let wav = a.out.join("audio.wav");
            let r = crate::xa_cmd::run(a.image, a.path, None, Some(ch), Some(&wav))?;
            audio["selected_channel"] = json!(ch);
            audio["wav"] = r["wav"].clone();
        }
    }

    let contact = if thumbs.is_empty() { None } else { Some(contact_sheet(a.out, &thumbs)?) };
    let frames_json = a.out.join("frames.json");
    std::fs::write(&frames_json, serde_json::to_string_pretty(&per_frame).unwrap_or_default())
        .map_err(|e| MediaError::io(&frames_json, e))?;
    let mut versions: BTreeMap<String, usize> = BTreeMap::new();
    for r in &per_frame {
        if let Some(v) = r["bs"]["version"].as_u64() {
            *versions.entry(format!("v{v}")).or_default() += 1;
        }
    }
    result["video"] = json!({
        "frames": frames_total,
        "dimensions": dims,
        "bs_variants": variants,
        "bs_versions_decoded": versions,
        "quant_range": if written.is_empty() { Value::Null } else { json!([quants.0, quants.1]) },
        "incomplete_frames": incomplete,
        "ff7_prefixed_frames": scan.frames.iter().filter(|f| bs_offset(&f.data) != 0).count(),
        "frames_without_end_code": no_end_code,
        "decoded": written.len(),
        "range": [first, last.min(frames_total)],
        "errors": errors,
        "error_samples": error_samples,
        "fps": fps,
    });
    result["audio"] = audio;
    let summary = a.out.join("summary.json");
    result["out"] = json!({
        "dir": a.out,
        "first_png": written.first().map(|i| format!("frame_{i:05}.png")),
        "last_png": written.last().map(|i| format!("frame_{i:05}.png")),
        "frames_json": frames_json,
        "contact_sheet": contact,
        "y4m": a.y4m.then(|| a.out.join("movie.y4m")),
        "summary": summary,
    });
    std::fs::write(&summary, serde_json::to_string_pretty(&result).unwrap_or_default()).map_err(|e| MediaError::io(&summary, e))?;
    Ok(result)
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// Append one frame to `movie.y4m` (4:4:4, BT.601 full range). Frames of a
/// different size than the first are skipped.
fn write_y4m(
    state: &mut Option<(std::io::BufWriter<std::fs::File>, (u16, u16))>,
    dir: &Path,
    w: u16,
    h: u16,
    fps: f64,
    rgb: &[u8],
) -> psx_media::Result<()> {
    let p = dir.join("movie.y4m");
    let io = |e| MediaError::io(&p, e);
    if state.is_none() {
        let mut f = std::io::BufWriter::new(std::fs::File::create(&p).map_err(io)?);
        // The estimated rate, in thousandths.
        let milli = (fps * 1000.0).round().max(1.0) as u64;
        writeln!(f, "YUV4MPEG2 W{w} H{h} F{milli}:1000 Ip A1:1 C444 XCOLORRANGE=FULL").map_err(io)?;
        *state = Some((f, (w, h)));
    }
    let (f, dims) = state.as_mut().expect("y4m state");
    if *dims != (w, h) {
        return Ok(());
    }
    let n = w as usize * h as usize;
    let mut planes = vec![0u8; n * 3];
    for i in 0..n {
        let (r, g, b) = (rgb[i * 3] as f64, rgb[i * 3 + 1] as f64, rgb[i * 3 + 2] as f64);
        let y = 0.299 * r + 0.587 * g + 0.114 * b;
        planes[i] = y.round().clamp(0.0, 255.0) as u8;
        planes[n + i] = (128.0 + (b - y) * 0.564).round().clamp(0.0, 255.0) as u8;
        planes[2 * n + i] = (128.0 + (r - y) * 0.713).round().clamp(0.0, 255.0) as u8;
    }
    f.write_all(b"FRAME\n").map_err(io)?;
    f.write_all(&planes).map_err(io)
}

/// The given frames at half size in a 6-wide grid.
fn contact_sheet(dir: &Path, frames: &[(usize, u16, u16, Vec<u8>)]) -> psx_media::Result<PathBuf> {
    let count = frames.len();
    let picks: Vec<&(usize, u16, u16, Vec<u8>)> = frames.iter().collect();
    let tw = picks.iter().map(|f| f.1 as usize).max().unwrap_or(16) / 2;
    let th = picks.iter().map(|f| f.2 as usize).max().unwrap_or(16) / 2;
    let cols = count.min(6);
    let rows = count.div_ceil(cols);
    let (sw, sh) = (cols * (tw + 2), rows * (th + 2));
    let mut sheet = vec![32u8; sw * sh * 3];
    for (n, (_, w, h, rgb)) in picks.iter().map(|f| (f.0, f.1, f.2, &f.3)).enumerate() {
        let (ox, oy) = ((n % cols) * (tw + 2) + 1, (n / cols) * (th + 2) + 1);
        for y in 0..(h as usize / 2) {
            for x in 0..(w as usize / 2) {
                for c in 0..3 {
                    let s = |dx: usize, dy: usize| rgb[((y * 2 + dy) * w as usize + x * 2 + dx) * 3 + c] as u32;
                    let v = (s(0, 0) + s(1, 0) + s(0, 1) + s(1, 1) + 2) / 4;
                    sheet[((oy + y) * sw + ox + x) * 3 + c] = v as u8;
                }
            }
        }
    }
    let p = dir.join("contact.png");
    crate::run::write_png(&p, sw as u32, sh as u32, &sheet, png::ColorType::Rgb).map_err(|e| MediaError::io(&p, e))?;
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Orthonormal 8×8 IDCT in f64: `out[y][x] = Σ c(u)c(v)/4 · F[v][u] cos.. cos..`.
    fn reference_idct(coef: &[f64; 64]) -> [f64; 64] {
        let c = |u: usize| if u == 0 { std::f64::consts::FRAC_1_SQRT_2 } else { 1.0 };
        let mut out = [0.0; 64];
        for y in 0..8 {
            for x in 0..8 {
                let mut s = 0.0;
                for v in 0..8 {
                    for u in 0..8 {
                        let cx = (((2 * x + 1) * u) as f64 * std::f64::consts::PI / 16.0).cos();
                        let cy = (((2 * y + 1) * v) as f64 * std::f64::consts::PI / 16.0).cos();
                        s += c(u) * c(v) / 4.0 * coef[v * 8 + u] * cx * cy;
                    }
                }
                out[y * 8 + x] = s;
            }
        }
        out
    }

    /// Run-length encode a zig-zag coefficient list as MDEC halfwords.
    fn rle(quant: u16, zz: &[i32; 64]) -> Vec<u16> {
        let mut out = vec![(quant << 10) | (zz[0] as u16 & 0x3FF)];
        let mut run = 0u16;
        for &v in &zz[1..] {
            if v == 0 {
                run += 1;
            } else {
                out.push((run << 10) | (v as u16 & 0x3FF));
                run = 0;
            }
        }
        out.push(0xFE00);
        out
    }

    #[test]
    fn hardware_model_idct_matches_float_reference() {
        let mut m = standard_mdec();
        let q = 2u16;
        // Luma block with a handful of AC terms; chroma flat (Cr=Cb=0 so R=G=B=Y+128).
        let mut zz = [0i32; 64];
        zz[0] = -40;
        zz[1] = 12;
        zz[2] = -7;
        zz[5] = 4;
        zz[9] = 3;
        zz[20] = -2;
        let mut coef = [0f64; 64];
        let dq = |k: usize, v: i32| {
            if k == 0 {
                (v * DEFAULT_QUANT[0] as i32) as f64
            } else {
                ((v * DEFAULT_QUANT[ZAGZIG[k]] as i32 * q as i32 + 4) / 8) as f64
            }
        };
        for k in 0..64 {
            coef[ZAGZIG[k]] = dq(k, zz[k]);
        }
        let reference = reference_idct(&coef);
        let flat = rle(q, &[0; 64]);
        let mut hw: Vec<u16> = flat.iter().chain(&flat).copied().collect();
        for _ in 0..4 {
            hw.extend(rle(q, &zz));
        }
        let rgb = decode_macroblock(&mut m, &hw);
        let mut worst = 0f64;
        for y in 0..8 {
            for x in 0..8 {
                let want = (reference[y * 8 + x] + 128.0).round().clamp(0.0, 255.0);
                for c in 0..3 {
                    worst = worst.max((rgb[(y * 16 + x) * 3 + c] as f64 - want).abs());
                }
            }
        }
        assert!(worst <= 2.0, "max deviation {worst}");
    }

    #[test]
    fn ycbcr_to_rgb_uses_bt601_weights() {
        let mut m = standard_mdec();
        // DC-only blocks: a flat block's value is DC * q[0] / 8. The core's
        // integer IDCT rounds negative sums towards zero, so allow one step of
        // block error times the largest weight (1.772).
        let dc = |v: i32| {
            let mut zz = [0i32; 64];
            zz[0] = v;
            rle(1, &zz)
        };
        let mut hw = dc(40);
        hw.extend(dc(-24));
        for _ in 0..4 {
            hw.extend(dc(0));
        }
        let rgb = decode_macroblock(&mut m, &hw);
        // Flat block value = DC*q0/8 = DC*2/8.
        let (cr, cb) = (40.0 * 2.0 / 8.0, -24.0 * 2.0 / 8.0);
        let want = [128.0 + 1.402 * cr, 128.0 - 0.3437 * cb - 0.7143 * cr, 128.0 + 1.772 * cb];
        for p in rgb.chunks(3) {
            for c in 0..3 {
                assert!((p[c] as f64 - want[c]).abs() <= 2.0, "{p:?} vs {want:?}");
            }
        }
    }

    #[test]
    fn frame_ranges() {
        assert_eq!(parse_range("3..7").unwrap(), (3, 7));
        assert_eq!(parse_range("5").unwrap(), (5, 5));
        assert_eq!(parse_range("..4").unwrap(), (1, 4));
        assert_eq!(parse_range("10..").unwrap(), (10, usize::MAX));
        assert!(parse_range("0..2").is_err() && parse_range("5..2").is_err());
    }

    #[test]
    fn sources() {
        assert_eq!(split_source("a.cue::/X.STR", None).unwrap(), (PathBuf::from("a.cue"), "/X.STR".into()));
        assert_eq!(split_source("a.cue", Some("/X.STR")).unwrap(), (PathBuf::from("a.cue"), "/X.STR".into()));
        assert!(split_source("a.cue", None).is_err());
    }
}
