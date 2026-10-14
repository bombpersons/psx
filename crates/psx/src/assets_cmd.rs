//! `psx assets scan|extract`: find TIM/TMD/VAB/VAG data in disc files,
//! archives or RAM dumps, and convert it to PNG / glTF / WAV.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use psx_assets::gltf::{texture_keys, tmd_to_gltf, TextureKey};
use psx_assets::scan::{self, Hit, Info, Kind, Options};
use psx_assets::vab::{levels, wav_mono};
use psx_assets::{adpcm, Tim, TimAlpha, Tmd, Vab, Vag, Vram};
use psx_media::iso9660::FileSystem;
use psx_media::{Disc, MediaError};
use serde_json::{json, Value};

type R<T> = psx_media::Result<T>;

/// RAM dumps (`ram.bin` from runs) start at KSEG0.
const RAM_BASE: u32 = 0x8000_0000;

/// One byte source to scan: a disc file or a host file.
pub struct Item {
    /// `/DIR/FILE` on a disc, or the host path.
    pub name: String,
    pub bytes: Vec<u8>,
    pub ram_base: Option<u32>,
    /// `(name, offset, size)` entries when a sibling index (e.g. Croc's
    /// `CROCFILE.DIR`) describes this file as an archive.
    pub index: Vec<(String, usize, usize)>,
    /// A separate `.VB` body for a lone `.VH` at offset 0, when a sibling exists.
    pub sibling_vb: Option<Vec<u8>>,
}

fn is_disc_image(p: &Path) -> bool {
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "cue" | "bin" | "iso"))
}

fn with_ext(path: &str, ext: &str) -> String {
    match path.rsplit_once('.') {
        Some((stem, _)) if !stem.ends_with('/') => format!("{stem}.{ext}"),
        _ => format!("{path}.{ext}"),
    }
}

/// Croc-style directory: `u32 count`, then `count` x {char name[12]; u32 size, offset, zero}`,
/// covering the archive exactly. Returns `None` unless it validates against `archive_len`.
fn parse_dir_index(dir: &[u8], archive_len: usize) -> Option<Vec<(String, usize, usize)>> {
    let n = u32::from_le_bytes(dir.get(..4)?.try_into().ok()?) as usize;
    if n == 0 || 4 + n * 24 > dir.len() {
        return None;
    }
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let e = &dir[4 + i * 24..4 + (i + 1) * 24];
        let name: String = e[..12].iter().take_while(|&&c| c != 0).map(|&c| c as char).collect();
        let w = |k: usize| u32::from_le_bytes(e[k..k + 4].try_into().unwrap()) as usize;
        let (size, offset) = (w(12), w(16));
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_graphic()) || offset + size > archive_len {
            return None;
        }
        out.push((name, offset, size));
    }
    Some(out)
}

/// Resolve a source: `image.cue::/PATH` (a file, `/DIR/` for a directory,
/// or `*` for every file), a bare disc image (every file), or a host file.
pub fn items(source: &str, ram_base: Option<u32>) -> R<Vec<Item>> {
    let (image, pattern) = match source.split_once("::") {
        Some((i, p)) => (PathBuf::from(i), Some(p.to_string())),
        None if is_disc_image(Path::new(source)) && Disc::open(source).is_ok() => (PathBuf::from(source), None),
        None => {
            let bytes = std::fs::read(source).map_err(|e| MediaError::io(source, e))?;
            let p = Path::new(source);
            let looks_like_ram = p.file_name().is_some_and(|n| n == "ram.bin") || bytes.len() == 2 << 20;
            let vb = p.extension().is_some_and(|e| e.eq_ignore_ascii_case("vh")).then(|| std::fs::read(p.with_extension("VB")).or_else(|_| std::fs::read(p.with_extension("vb"))).ok()).flatten();
            return Ok(vec![Item {
                name: source.to_string(),
                bytes,
                ram_base: ram_base.or(looks_like_ram.then_some(RAM_BASE)),
                index: Vec::new(),
                sibling_vb: vb,
            }]);
        }
    };
    let disc = Disc::open(&image)?;
    let fs = FileSystem::read(&disc)?;
    let pattern = pattern.unwrap_or_else(|| "*".into());
    let files: Vec<_> = fs.entries.iter().filter(|e| !e.is_dir).collect();
    let selected: Vec<_> = if pattern == "*" || pattern == "/" || pattern == "/*" {
        files
    } else if pattern.ends_with('/') || pattern.ends_with("/*") {
        let dir = psx_media::iso9660::normalize(pattern.trim_end_matches('*'));
        let dir = if dir.ends_with('/') { dir } else { format!("{dir}/") };
        files.into_iter().filter(|e| e.path.to_ascii_uppercase().starts_with(&dir.to_ascii_uppercase())).collect()
    } else {
        vec![fs.find(&pattern).ok_or_else(|| MediaError::NotFound(pattern.clone()))?]
    };
    let read = |path: &str| fs.find(path).filter(|e| !e.is_dir).and_then(|e| fs.read_file(&disc, e).ok());
    let mut out = Vec::new();
    for e in selected {
        // Form 2 (XA/STR) files hold audio/video, not these formats.
        let Ok(bytes) = fs.read_file(&disc, e) else { continue };
        let index = read(&with_ext(&e.path, "DIR")).filter(|_| !e.path.to_ascii_uppercase().ends_with(".DIR")).and_then(|d| parse_dir_index(&d, bytes.len())).unwrap_or_default();
        let sibling_vb = e.path.to_ascii_uppercase().ends_with(".VH").then(|| read(&with_ext(&e.path, "VB"))).flatten();
        out.push(Item { name: e.path.clone(), bytes, ram_base, index, sibling_vb });
    }
    Ok(out)
}

fn entry_of(item: &Item, offset: usize) -> Option<(&str, usize)> {
    item.index.iter().find(|(_, o, s)| offset >= *o && offset < o + s).map(|(n, o, _)| (n.as_str(), offset - o))
}

fn hit_json(item: &Item, h: &Hit) -> Value {
    let mut v = serde_json::to_value(h).unwrap();
    v["offset"] = json!(format!("0x{:X}", h.offset));
    if let Some((name, off)) = entry_of(item, h.offset) {
        v["entry"] = json!({"name": name, "offset": format!("0x{off:X}")});
    }
    if let (Some(base), Info::Tmd { .. }) = (item.ram_base, &h.info) {
        v["address"] = json!(format!("0x{:08X}", base as usize + h.offset));
    }
    v
}

fn describe(h: &Hit) -> String {
    match &h.info {
        Info::Tim { bpp, width, height, palettes, image_vram, .. } => {
            format!("{bpp}bpp {width}x{height} palettes={palettes} vram=({},{})", image_vram[0], image_vram[1])
        }
        Info::Tmd { objects, vertices, polygons, textured, skipped, fixp } => {
            format!("objects={objects} vertices={vertices} polygons={polygons} textured={textured} skipped={skipped}{}", if *fixp { " fixp" } else { "" })
        }
        Info::Vab { programs, tones, waveforms, body_included, .. } => {
            format!("programs={programs} tones={tones} waveforms={waveforms}{}", if *body_included { "" } else { " (VH only)" })
        }
        Info::Vag { rate, name, data_size } => format!("{rate} Hz {name:?} {data_size} bytes"),
    }
}

#[derive(clap::Subcommand)]
pub enum AssetsAction {
    /// List TIM/TMD/VAB/VAG data found by signature + validation. SOURCE is
    /// `image.cue::/PATH` (a file; `/DIR/` or `*` for many), a disc image (every
    /// file) or a host file (`ram.bin` dumps resolve relocated TMDs at 0x80000000).
    Scan {
        source: String,
        /// One JSON document instead of a text table.
        #[arg(long)]
        json: bool,
        /// Only these kinds (tim, tmd, vab, vag); repeatable.
        #[arg(long = "kind")]
        kinds: Vec<String>,
        /// Address of the file's first byte when it is a RAM dump (hex).
        #[arg(long)]
        ram_base: Option<String>,
    },
    /// Convert what `scan` finds: TIM -> PNG per CLUT, TMD -> glTF (+ texture
    /// page PNGs from TIMs in the same file or --textures), VAB/VAG -> WAV.
    /// Original bytes are written next to each conversion.
    Extract {
        source: String,
        #[arg(short, long)]
        out: PathBuf,
        #[arg(long = "kind")]
        kinds: Vec<String>,
        /// Only the asset at this file offset (hex).
        #[arg(long)]
        offset: Option<String>,
        /// More TIM sources (same syntax as SOURCE) uploaded to the virtual VRAM
        /// that TMD texture pages are decoded from; repeatable.
        #[arg(long = "textures")]
        textures: Vec<String>,
        #[arg(long)]
        ram_base: Option<String>,
    },
}

fn hex(s: &Option<String>) -> R<Option<u32>> {
    s.as_deref()
        .map(|v| u32::from_str_radix(v.trim_start_matches("0x").trim_start_matches("0X"), 16).map_err(|_| MediaError::Malformed(format!("bad hex value {v}"))))
        .transpose()
}

fn kinds(k: &[String]) -> R<Vec<Kind>> {
    k.iter().map(|s| Kind::parse(s).ok_or_else(|| MediaError::Malformed(format!("unknown kind {s} (tim, tmd, vab, vag)")))).collect()
}

/// Run an `assets` subcommand: `Ok(Ok(json))`, `Ok(Err(text))` for the text table.
pub fn run(action: AssetsAction) -> R<Result<Value, String>> {
    match action {
        AssetsAction::Scan { source, json, kinds: k, ram_base } => {
            scan_cmd(ScanArgs { source: &source, json, kinds: &kinds(&k)?, ram_base: hex(&ram_base)? })
        }
        AssetsAction::Extract { source, out, kinds: k, offset, textures, ram_base } => extract_cmd(ExtractArgs {
            source: &source,
            out: &out,
            kinds: &kinds(&k)?,
            offset: hex(&offset)?.map(|o| o as usize),
            textures: &textures,
            ram_base: hex(&ram_base)?,
        })
        .map(Ok),
    }
}

pub struct ScanArgs<'a> {
    pub source: &'a str,
    pub json: bool,
    pub kinds: &'a [Kind],
    pub ram_base: Option<u32>,
}

/// Returns the JSON report, or a text table when `json` is false.
pub fn scan_cmd(a: ScanArgs) -> R<Result<Value, String>> {
    let items = items(a.source, a.ram_base)?;
    let mut files = Vec::new();
    let mut totals: BTreeMap<Kind, usize> = BTreeMap::new();
    let mut text = String::new();
    for item in &items {
        let hits: Vec<Hit> = scan::scan(&item.bytes, Options { ram_base: item.ram_base }).into_iter().filter(|h| a.kinds.is_empty() || a.kinds.contains(&h.kind)).collect();
        if hits.is_empty() {
            continue;
        }
        for h in &hits {
            *totals.entry(h.kind).or_default() += 1;
            let entry = entry_of(item, h.offset).map(|(n, o)| format!("  [{n}+0x{o:X}]")).unwrap_or_default();
            text.push_str(&format!("{}  0x{:08X}  {:<3}  {:>8}  {}{entry}\n", item.name, h.offset, h.kind.ext(), h.size, describe(h)));
        }
        files.push(json!({"file": item.name, "bytes": item.bytes.len(), "hits": hits.iter().map(|h| hit_json(item, h)).collect::<Vec<_>>()}));
    }
    if !a.json {
        text.push_str(&format!("{} files scanned, totals {:?}\n", items.len(), totals));
        return Ok(Err(text));
    }
    Ok(Ok(json!({"source": a.source, "files_scanned": items.len(), "totals": totals.iter().map(|(k, n)| (k.ext().to_string(), json!(n))).collect::<serde_json::Map<_, _>>(), "files": files})))
}

pub struct ExtractArgs<'a> {
    pub source: &'a str,
    pub out: &'a Path,
    pub kinds: &'a [Kind],
    /// Only the hit at this offset.
    pub offset: Option<usize>,
    /// Extra TIM sources uploaded to the virtual VRAM before each item's own TIMs.
    pub textures: &'a [String],
    pub ram_base: Option<u32>,
}

fn safe_name(s: &str) -> String {
    s.trim_start_matches(['/', '.']).chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' { c } else { '_' }).collect()
}

fn write(path: &Path, bytes: &[u8]) -> R<()> {
    std::fs::write(path, bytes).map_err(|e| MediaError::io(path, e))
}

fn png(path: &Path, w: u32, h: u32, rgba: &[u8]) -> R<()> {
    crate::run::write_png(path, w, h, rgba, png::ColorType::Rgba).map_err(|e| MediaError::io(path, e))
}

fn wav_stats(pcm: &[i16], rate: u32) -> Value {
    let (peak, rms) = levels(pcm);
    let clipped = pcm.iter().filter(|&&s| s == i16::MAX || s == i16::MIN).count();
    json!({"samples": pcm.len(), "rate": rate, "seconds": (pcm.len() as f64 / rate as f64 * 1000.0).round() / 1000.0,
           "peak": (peak * 1000.0).round() / 1000.0, "rms": (rms * 10000.0).round() / 10000.0, "clipped_samples": clipped})
}

fn load_tims(vram: &mut Vram, bytes: &[u8]) -> usize {
    let tims = Tim::find_all(bytes);
    for (_, t) in &tims {
        vram.load_tim(t);
    }
    tims.len()
}

pub fn extract_cmd(a: ExtractArgs) -> R<Value> {
    let mut base_vram = Vram::default();
    let mut texture_tims = 0;
    for t in a.textures {
        for it in items(t, None)? {
            texture_tims += load_tims(&mut base_vram, &it.bytes);
        }
    }
    let items = items(a.source, a.ram_base)?;
    let mut report = Vec::new();
    let want = |k: Kind| a.kinds.is_empty() || a.kinds.contains(&k);
    for item in &items {
        let hits: Vec<Hit> = scan::scan(&item.bytes, Options { ram_base: item.ram_base })
            .into_iter()
            .filter(|h| want(h.kind) && a.offset.is_none_or(|o| o == h.offset))
            .collect();
        if hits.is_empty() {
            continue;
        }
        let dir = a.out.join(safe_name(&item.name));
        std::fs::create_dir_all(&dir).map_err(|e| MediaError::io(&dir, e))?;
        // VRAM for this item's models: the --textures TIMs, then this item's own.
        let mut vram = base_vram.clone();
        let own_tims = if hits.iter().any(|h| h.kind == Kind::Tmd) { load_tims(&mut vram, &item.bytes) } else { 0 };
        for h in &hits {
            let stem = match entry_of(item, h.offset) {
                Some((name, 0)) => safe_name(name),
                Some((name, off)) => format!("{}_{off:X}", safe_name(name)),
                None => format!("{:08X}", h.offset),
            };
            let bytes = &item.bytes[h.offset..];
            let mut outputs = Vec::new();
            if h.size > 0 {
                let p = dir.join(format!("{stem}.{}", h.kind.ext()));
                write(&p, &bytes[..h.size])?;
                outputs.push(json!({"path": p, "role": "original"}));
            }
            let mut extra = json!({});
            match h.kind {
                Kind::Tim => {
                    let t = Tim::parse(bytes).map_err(MediaError::Malformed)?;
                    let n = t.palette_count().max(1);
                    for pal in 0..n {
                        let p = if n == 1 { dir.join(format!("{stem}.png")) } else { dir.join(format!("{stem}_p{pal}.png")) };
                        png(&p, t.width(), t.height(), &t.to_rgba(pal, TimAlpha::Cutout))?;
                        outputs.push(json!({"path": p, "palette": pal}));
                    }
                }
                Kind::Tmd => {
                    let t = Tmd::parse_at(&item.bytes, h.offset, item.ram_base).map_err(MediaError::Malformed)?;
                    let mut pages = BTreeMap::new();
                    for key in texture_keys(&t) {
                        let (rgba, cover) = vram.decode_page(key.page, key.clut.unwrap_or((0, 0)), TimAlpha::Cutout);
                        let file = format!("{stem}_{}.png", key.label());
                        if cover > 0.0 {
                            png(&dir.join(&file), 256, 256, &rgba)?;
                        }
                        pages.insert(key, (file, cover));
                    }
                    let uri = |k: &TextureKey| pages.get(k).filter(|(_, c)| *c > 0.0).map(|(f, _)| f.clone());
                    let doc = tmd_to_gltf(&t, &stem, &uri);
                    let p = dir.join(format!("{stem}.gltf"));
                    write(&p, serde_json::to_string(&doc).unwrap().as_bytes())?;
                    outputs.push(json!({"path": p, "role": "gltf"}));
                    extra = json!({
                        "vram_tims": {"from_textures": texture_tims, "from_this_file": own_tims},
                        "texture_pages": pages.iter().map(|(k, (f, c))| json!({
                            "page": [k.page.x, k.page.y], "bits": k.page.bits, "clut": k.clut.map(|c| [c.0, c.1]),
                            "png": (*c > 0.0).then(|| dir.join(f)), "vram_coverage": (c * 1000.0).round() / 1000.0,
                        })).collect::<Vec<_>>(),
                    });
                }
                Kind::Vag => {
                    let v = Vag::parse(bytes).map_err(MediaError::Malformed)?;
                    let d = v.decode(bytes);
                    let p = dir.join(format!("{stem}.wav"));
                    write(&p, &wav_mono(&d.pcm, v.rate))?;
                    outputs.push(json!({"path": p, "wav": wav_stats(&d.pcm, v.rate), "loop": d.looped}));
                }
                Kind::Vab => {
                    let v = Vab::parse(bytes).map_err(MediaError::Malformed)?;
                    let body: Option<&[u8]> = if let Some(at) = v.body_offset(bytes) {
                        Some(&bytes[at..at + v.body_size])
                    } else if h.offset == 0 {
                        item.sibling_vb.as_deref().filter(|vb| v.body_matches(vb))
                    } else {
                        None
                    };
                    let p = dir.join(format!("{stem}.vab.json"));
                    write(&p, serde_json::to_string_pretty(&v).unwrap().as_bytes())?;
                    outputs.push(json!({"path": p, "role": "programs"}));
                    match body {
                        Some(body) => {
                            for (i, r) in v.vag_ranges().into_iter().enumerate() {
                                let Some(data) = body.get(r) else { break };
                                let d = adpcm::decode_stream(data);
                                // At the tone's centre note the SPU plays a waveform at 44.1 kHz.
                                let p = dir.join(format!("{stem}_w{:02}.wav", i + 1));
                                write(&p, &wav_mono(&d.pcm, 44_100))?;
                                outputs.push(json!({"path": p, "waveform": i + 1, "wav": wav_stats(&d.pcm, 44_100), "loop": d.looped}));
                            }
                        }
                        None => extra = json!({"warning": "VH without a matching VB body (the waveforms are stored elsewhere); a .VB with the same name next to a .VH is used automatically"}),
                    }
                }
            }
            let mut entry = hit_json(item, h);
            entry["outputs"] = json!(outputs);
            if extra.as_object().is_some_and(|o| !o.is_empty()) {
                entry["details"] = extra;
            }
            report.push(entry);
        }
    }
    Ok(json!({"source": a.source, "out": a.out, "extracted": report.len(), "assets": report}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn croc_style_dir_index_validates_against_archive() {
        let mut d = 2u32.to_le_bytes().to_vec();
        for (name, size, off) in [("A.BIN", 10u32, 0u32), ("B.WAD", 6, 10)] {
            let mut n = [0u8; 12];
            n[..name.len()].copy_from_slice(name.as_bytes());
            d.extend_from_slice(&n);
            for w in [size, off, 0] {
                d.extend_from_slice(&w.to_le_bytes());
            }
        }
        let idx = parse_dir_index(&d, 16).unwrap();
        assert_eq!(idx[1], ("B.WAD".to_string(), 10, 6));
        assert!(parse_dir_index(&d, 15).is_none());
        assert_eq!(with_ext("/X/CROCFILE.1", "DIR"), "/X/CROCFILE.DIR");
    }
}
