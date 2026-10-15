//! `psx assets scan|extract`: find TIM/TMD/VAB/VAG data in disc files,
//! archives or RAM dumps, and convert it to PNG / glTF / WAV.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::parser::{self, Parsers};
use psx_assets::archive::{Archive, Entry};
use psx_assets::gltf::{texture_keys, tmd_to_gltf, TextureKey};
use psx_assets::model_document::ModelDocument;
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
    pub archive: Option<Archive>,
    pub archive_parser: Option<Value>,
    pub parsed: Option<Vec<ParsedAsset>>,
    /// A separate `.VB` body for a lone `.VH` at offset 0, when a sibling exists.
    pub sibling_vb: Option<Vec<u8>>,
}

pub struct ParsedAsset {
    hit: Hit,
    model: Option<Tmd>,
    normalized: Option<Vec<u8>>,
    document: Vec<u8>,
    provenance: Value,
}

fn finish_item(mut item: Item, parsers: &Parsers) -> R<Item> {
    if let Some(spec) = &parsers.archive {
        let output = parser::run(spec, &item.bytes, "archive", &[("PSX_SOURCE_FILE", item.name.clone())], parsers.cancel.as_ref())?;
        item.archive = Some(parser::document::<parser::ArchiveDocument>(&output.bytes)?.validate(item.bytes.len())?);
        item.archive_parser = Some(output.provenance);
    }
    let Some((spec, kind)) = parsers.model.as_ref().map(|s| (s, "model")).or_else(|| parsers.asset.as_ref().map(|s| (s, "asset"))) else {
        return Ok(item);
    };
    let spans: Vec<_> = match &item.archive {
        Some(archive) => archive.entries.iter().filter(|e| e.size != 0).map(|e| (e.offset, e.size, Some(e.index))).collect(),
        None => vec![(0, item.bytes.len(), None)],
    };
    let mut parsed = Vec::new();
    for (offset, size, index) in spans {
        let context = [
            ("PSX_SOURCE_FILE", item.name.clone()),
            ("PSX_ENTRY_INDEX", index.map(|i| i.to_string()).unwrap_or_default()),
            ("PSX_ENTRY_OFFSET", offset.to_string()),
        ];
        let output = parser::run(spec, &item.bytes[offset..offset + size], kind, &context, parsers.cancel.as_ref())?;
        let (hit, model, normalized) = if kind == "model" {
            let model = parser::document::<ModelDocument>(&output.bytes)?.into_model().map_err(MediaError::Malformed)?;
            (Hit { offset, size, kind: Kind::Tmd, info: scan::tmd_info(&model) }, Some(model), None)
        } else {
            let normalized = parser::document::<parser::PayloadDocument>(&output.bytes)?.decode()?;
            let hits = scan::scan(&normalized, Options::default());
            if hits.len() != 1 || hits[0].offset != 0 || hits[0].size == 0 {
                return Err(MediaError::Malformed("asset parser must return one validated standard TIM/TMD/VAB/VAG at offset zero".into()));
            }
            let mut hit = hits.into_iter().next().unwrap();
            hit.offset = offset;
            hit.size = size;
            (hit, None, Some(normalized))
        };
        parsed.push(ParsedAsset { hit, model, normalized, document: output.bytes, provenance: output.provenance });
    }
    item.parsed = Some(parsed);
    Ok(item)
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

fn archive_index(bytes: &[u8], directory: Option<&[u8]>, mode: Option<&str>, ram: bool) -> R<Option<Archive>> {
    match mode {
        Some("dot-t") => Archive::dot_t(bytes).map(Some).map_err(MediaError::Malformed),
        Some("none") => Ok(None),
        None if ram => Ok(None),
        None => Ok(directory.and_then(|d| Archive::croc_dir(d, bytes.len()).ok()).or_else(|| Archive::detect_dot_t(bytes))),
        Some(other) => Err(MediaError::Malformed(format!("unknown archive format {other:?} (dot-t, none)"))),
    }
}

/// Resolve a source: `image.cue::/PATH` (a file, `/DIR/` for a directory,
/// or `*` for every file), a bare disc image (every file), or a host file.
pub fn items(source: &str, ram_base: Option<u32>, archive_mode: Option<&str>, parsers: &Parsers) -> R<Vec<Item>> {
    let (image, pattern) = match source.split_once("::") {
        Some((i, p)) => (PathBuf::from(i), Some(p.to_string())),
        None if is_disc_image(Path::new(source)) && Disc::open(source).is_ok() => (PathBuf::from(source), None),
        None => {
            let bytes = std::fs::read(source).map_err(|e| MediaError::io(source, e))?;
            let p = Path::new(source);
            let looks_like_ram = p.file_name().is_some_and(|n| n == "ram.bin") || bytes.len() == 2 << 20;
            let vb = p
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("vh"))
                .then(|| std::fs::read(p.with_extension("VB")).or_else(|_| std::fs::read(p.with_extension("vb"))).ok())
                .flatten();
            let directory = (!p.extension().is_some_and(|e| e.eq_ignore_ascii_case("dir")))
                .then(|| std::fs::read(p.with_extension("DIR")).or_else(|_| std::fs::read(p.with_extension("dir"))).ok())
                .flatten();
            let archive = archive_index(
                &bytes,
                directory.as_deref(),
                archive_mode,
                ram_base.is_some() || p.file_name().is_some_and(|n| n == "ram.bin"),
            )?;
            let inferred_ram =
                (looks_like_ram && archive.is_none() && parsers.archive.is_none() && parsers.model.is_none() && parsers.asset.is_none())
                    .then_some(RAM_BASE);
            return Ok(vec![finish_item(
                Item {
                    name: source.to_string(),
                    bytes,
                    ram_base: ram_base.or(inferred_ram),
                    archive,
                    archive_parser: None,
                    parsed: None,
                    sibling_vb: vb,
                },
                parsers,
            )?]);
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
        let directory = (!e.path.to_ascii_uppercase().ends_with(".DIR")).then(|| read(&with_ext(&e.path, "DIR"))).flatten();
        let archive = archive_index(&bytes, directory.as_deref(), archive_mode, ram_base.is_some())?;
        let sibling_vb = e.path.to_ascii_uppercase().ends_with(".VH").then(|| read(&with_ext(&e.path, "VB"))).flatten();
        out.push(finish_item(
            Item { name: e.path.clone(), bytes, ram_base, archive, sibling_vb, archive_parser: None, parsed: None },
            parsers,
        )?);
    }
    Ok(out)
}

fn entry_of(item: &Item, offset: usize) -> Option<&Entry> {
    item.archive.as_ref()?.entries.iter().find(|entry| entry.contains(offset))
}

/// The bounded parent buffer used by the scanner and again by extraction.
fn asset_context(item: &Item, offset: usize) -> (&[u8], usize, Option<u32>) {
    match entry_of(item, offset) {
        Some(entry) => (
            entry.bytes(&item.bytes).unwrap(),
            offset - entry.offset,
            item.ram_base.and_then(|base| u32::try_from(entry.offset).ok().and_then(|o| base.checked_add(o))),
        ),
        None => (&item.bytes, offset, item.ram_base),
    }
}

fn item_hits(item: &Item) -> Vec<Hit> {
    if let Some(parsed) = &item.parsed {
        return parsed.iter().map(|p| p.hit.clone()).collect();
    }
    let Some(archive) = &item.archive else {
        return scan::scan(&item.bytes, Options { ram_base: item.ram_base });
    };
    let mut hits = Vec::new();
    for entry in &archive.entries {
        if entry.size == 0 {
            continue;
        }
        let bytes = entry.bytes(&item.bytes).unwrap();
        let ram_base = item.ram_base.and_then(|base| u32::try_from(entry.offset).ok().and_then(|o| base.checked_add(o)));
        for mut hit in scan::scan(bytes, Options { ram_base }) {
            hit.offset += entry.offset;
            hits.push(hit);
        }
    }
    hits
}

fn parsed_asset(item: &Item, offset: usize) -> Option<&ParsedAsset> {
    item.parsed.as_ref()?.iter().find(|p| p.hit.offset == offset)
}

fn decode_context(item: &Item, offset: usize) -> (&[u8], usize, Option<u32>) {
    match parsed_asset(item, offset).and_then(|p| p.normalized.as_deref()) {
        Some(bytes) => (bytes, 0, None),
        None => asset_context(item, offset),
    }
}

fn hit_json(item: &Item, h: &Hit) -> Value {
    let mut v = serde_json::to_value(h).unwrap();
    v["offset"] = json!(format!("0x{:X}", h.offset));
    if let Some(parsed) = parsed_asset(item, h.offset) {
        v["parser"] = parsed.provenance.clone();
    }
    if let Some(entry) = entry_of(item, h.offset) {
        let mut provenance = serde_json::to_value(entry).unwrap();
        provenance["archive_offset"] = json!(format!("0x{:X}", entry.offset));
        // Preserve Croc's existing relative `entry.offset` and optional name.
        provenance["offset"] = json!(format!("0x{:X}", h.offset - entry.offset));
        provenance["format"] = json!(item.archive.as_ref().unwrap().format);
        if let Some(parser) = &item.archive_parser {
            provenance["parser"] = parser.clone();
        }
        v["entry"] = provenance;
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
    /// List TIM/TMD/VAB/VAG data found by signature + validation.
    Scan(crate::commands::AssetsScanArgs),
    /// Convert what `scan` finds: TIM -> PNG per CLUT, TMD -> glTF (+ texture
    /// page PNGs from TIMs in the same file or --textures), VAB/VAG -> WAV.
    /// Original bytes are written next to each conversion. --entry extracts a
    /// complete raw archive entry without interpreting its payload.
    Extract(crate::commands::AssetsExtractArgs),
}

/// `assets scan`: `Ok(Ok(json))`, or `Ok(Err(text))` for the text table.
pub fn scan_assets(a: &crate::commands::AssetsScanArgs, ctx: &crate::commands::Ctx) -> R<Result<Value, String>> {
    let parsers = a.parsers.build(ctx, a.archive.as_deref())?;
    scan_cmd(ScanArgs {
        source: &a.source,
        json: a.json,
        kinds: &kinds(&a.kinds)?,
        ram_base: hex(&a.ram_base)?,
        archive: a.archive.as_deref(),
        parsers: &parsers,
    })
}

/// `assets extract`.
pub fn extract_assets(a: &crate::commands::AssetsExtractArgs, ctx: &crate::commands::Ctx) -> R<Value> {
    let parsers = a.parsers.build(ctx, a.archive.as_deref())?;
    extract_cmd(ExtractArgs {
        source: &a.source,
        out: &a.out,
        kinds: &kinds(&a.kinds)?,
        offset: hex(&a.offset)?.map(|o| o as usize),
        textures: &a.textures,
        ram_base: hex(&a.ram_base)?,
        archive: a.archive.as_deref(),
        entry: a.entry,
        parsers: &parsers,
    })
}

fn hex(s: &Option<String>) -> R<Option<u32>> {
    s.as_deref()
        .map(|v| {
            u32::from_str_radix(v.trim_start_matches("0x").trim_start_matches("0X"), 16)
                .map_err(|_| MediaError::Malformed(format!("bad hex value {v}")))
        })
        .transpose()
}

fn kinds(k: &[String]) -> R<Vec<Kind>> {
    k.iter().map(|s| Kind::parse(s).ok_or_else(|| MediaError::Malformed(format!("unknown kind {s} (tim, tmd, vab, vag)")))).collect()
}

pub struct ScanArgs<'a> {
    pub source: &'a str,
    pub json: bool,
    pub kinds: &'a [Kind],
    pub ram_base: Option<u32>,
    pub archive: Option<&'a str>,
    pub parsers: &'a Parsers,
}

/// Returns the JSON report, or a text table when `json` is false.
pub fn scan_cmd(a: ScanArgs) -> R<Result<Value, String>> {
    let items = items(a.source, a.ram_base, a.archive, a.parsers)?;
    let mut files = Vec::new();
    let mut totals: BTreeMap<Kind, usize> = BTreeMap::new();
    let mut text = String::new();
    for item in &items {
        let hits: Vec<Hit> = item_hits(item).into_iter().filter(|h| a.kinds.is_empty() || a.kinds.contains(&h.kind)).collect();
        if let Some(archive) = &item.archive {
            text.push_str(&format!(
                "{}  archive {:?}: {} entries ({} empty)\n",
                item.name,
                archive.format,
                archive.entries.len(),
                archive.entries.iter().filter(|e| e.size == 0).count()
            ));
        }
        if hits.is_empty() && item.archive.is_none() {
            continue;
        }
        for h in &hits {
            *totals.entry(h.kind).or_default() += 1;
            let entry = entry_of(item, h.offset)
                .map(|e| format!("  [{}+0x{:X}]", e.name.clone().unwrap_or_else(|| format!("entry {}", e.index)), h.offset - e.offset))
                .unwrap_or_default();
            text.push_str(&format!("{}  0x{:08X}  {:<3}  {:>8}  {}{entry}\n", item.name, h.offset, h.kind.ext(), h.size, describe(h)));
        }
        let mut file =
            json!({"file": item.name, "bytes": item.bytes.len(), "hits": hits.iter().map(|h| hit_json(item, h)).collect::<Vec<_>>()});
        if let Some(archive) = &item.archive {
            file["archive"] = json!(archive);
            if let Some(parser) = &item.archive_parser {
                file["archive"]["parser"] = parser.clone();
            }
        }
        files.push(file);
    }
    if !a.json {
        text.push_str(&format!("{} files scanned, totals {:?}\n", items.len(), totals));
        return Ok(Err(text));
    }
    Ok(Ok(
        json!({"source": a.source, "files_scanned": items.len(), "totals": totals.iter().map(|(k, n)| (k.ext().to_string(), json!(n))).collect::<serde_json::Map<_, _>>(), "files": files}),
    ))
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
    pub archive: Option<&'a str>,
    /// Raw entry extraction, without interpreting its payload.
    pub entry: Option<usize>,
    pub parsers: &'a Parsers,
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

fn load_item_tims(vram: &mut Vram, item: &Item) -> usize {
    let mut count = 0;
    for hit in item_hits(item).into_iter().filter(|h| h.kind == Kind::Tim) {
        let (bytes, offset, _) = decode_context(item, hit.offset);
        if let Ok(tim) = Tim::parse(&bytes[offset..]) {
            vram.load_tim(&tim);
            count += 1;
        }
    }
    count
}

pub fn extract_cmd(a: ExtractArgs) -> R<Value> {
    if a.entry.is_some()
        && (a.offset.is_some() || !a.kinds.is_empty() || !a.textures.is_empty() || a.parsers.model.is_some() || a.parsers.asset.is_some())
    {
        return Err(MediaError::Malformed("raw --entry extraction cannot combine with asset selection or model/asset parsers".into()));
    }
    let items = items(a.source, a.ram_base, a.archive, a.parsers)?;
    if let Some(index) = a.entry {
        if items.len() != 1 {
            return Err(MediaError::Malformed("--entry requires a single source file".into()));
        }
        let item = &items[0];
        let archive = item.archive.as_ref().ok_or_else(|| {
            MediaError::Malformed("source is not a recognized archive (use --archive dot-t for explicit validation)".into())
        })?;
        let entry = archive
            .entries
            .get(index)
            .ok_or_else(|| MediaError::Malformed(format!("archive entry {index} out of range ({} entries)", archive.entries.len())))?;
        let dir = a.out.join(safe_name(&item.name));
        std::fs::create_dir_all(&dir).map_err(|e| MediaError::io(&dir, e))?;
        let name = entry.name.as_deref().map(safe_name).unwrap_or_else(|| "data.bin".into());
        let path = dir.join(format!("entry_{index:04}_{name}"));
        write(&path, entry.bytes(&item.bytes).unwrap())?;
        return Ok(json!({"source": a.source, "out": a.out, "extracted": 1, "assets": [],
            "entries": [{"file": item.name, "format": archive.format, "entry": entry, "parser":item.archive_parser,
                         "outputs": [{"path": path, "role": "archive_entry"}]}]}));
    }
    let mut base_vram = Vram::default();
    let mut texture_tims = 0;
    for t in a.textures {
        for it in self::items(t, None, None, &Parsers::default())? {
            texture_tims += load_item_tims(&mut base_vram, &it);
        }
    }
    let mut report = Vec::new();
    let want = |k: Kind| a.kinds.is_empty() || a.kinds.contains(&k);
    for item in &items {
        let hits: Vec<Hit> = item_hits(item).into_iter().filter(|h| want(h.kind) && a.offset.is_none_or(|o| o == h.offset)).collect();
        if hits.is_empty() {
            continue;
        }
        let dir = a.out.join(safe_name(&item.name));
        std::fs::create_dir_all(&dir).map_err(|e| MediaError::io(&dir, e))?;
        // VRAM for this item's models: the --textures TIMs, then this item's own.
        let mut vram = base_vram.clone();
        let own_tims = if hits.iter().any(|h| h.kind == Kind::Tmd) { load_item_tims(&mut vram, item) } else { 0 };
        for h in &hits {
            let stem = match entry_of(item, h.offset) {
                Some(entry) => {
                    let name = match (&entry.name, item.archive.as_ref().unwrap().format) {
                        (Some(name), psx_assets::archive::Format::External) => format!("entry_{:04}_{}", entry.index, safe_name(name)),
                        (Some(name), _) => safe_name(name),
                        (None, _) => format!("entry_{:04}", entry.index),
                    };
                    let off = h.offset - entry.offset;
                    if off == 0 {
                        name
                    } else {
                        format!("{name}_{off:X}")
                    }
                }
                None => format!("{:08X}", h.offset),
            };
            let (source_parent, source_offset, _) = asset_context(item, h.offset);
            let (parent, local_offset, ram_base) = decode_context(item, h.offset);
            let bytes = &parent[local_offset..];
            let parsed = parsed_asset(item, h.offset);
            let mut outputs = Vec::new();
            if h.size > 0 {
                let extension = if parsed.is_some() { "source.bin" } else { h.kind.ext() };
                let p = dir.join(format!("{stem}.{extension}"));
                write(&p, &source_parent[source_offset..source_offset + h.size])?;
                outputs.push(json!({"path": p, "role": "original"}));
            }
            if let Some(parsed) = parsed {
                let p = dir.join(format!("{stem}.parser.json"));
                write(&p, &parsed.document)?;
                outputs.push(json!({"path":p,"role":"parser_document"}));
                if let Some(normalized) = &parsed.normalized {
                    let p = dir.join(format!("{stem}.{}", h.kind.ext()));
                    write(&p, normalized)?;
                    outputs.push(json!({"path":p,"role":"normalized_asset"}));
                }
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
                    let t = if let Some(model) = parsed.and_then(|p| p.model.as_ref()) {
                        model.clone()
                    } else {
                        Tmd::parse_at(parent, local_offset, ram_base).map_err(MediaError::Malformed)?
                    };
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
                        None => {
                            extra = json!({"warning": "VH without a matching VB body (the waveforms are stored elsewhere); a .VB with the same name next to a .VH is used automatically"})
                        }
                    }
                }
            }
            let mut entry = hit_json(item, h);
            entry["file"] = json!(item.name);
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

    fn tim() -> Vec<u8> {
        // Authored 2x2 direct-colour TIM, independent of any disc data.
        [
            16u32.to_le_bytes().as_slice(),
            2u32.to_le_bytes().as_slice(),
            20u32.to_le_bytes().as_slice(),
            &[0, 0, 0, 0, 2, 0, 2, 0],
            &[31, 0, 0, 0, 0, 0, 0, 0],
        ]
        .concat()
    }

    #[test]
    fn scanning_is_bounded_to_entries_and_reports_both_offsets() {
        let mut bytes = vec![0; 4 * 2048];
        bytes[..10].copy_from_slice(&[3, 0, 1, 0, 2, 0, 2, 0, 4, 0]);
        let tim = tim();
        // A valid TIM in the final entry, after an empty entry at the same offset.
        bytes[4096 + 32..4096 + 32 + tim.len()].copy_from_slice(&tim);
        // This second TIM crosses an entry boundary: the flat scanner accepts it.
        bytes[4096 - 16..4096 - 16 + tim.len()].copy_from_slice(&tim);
        let archive = Archive::detect_dot_t(&bytes).unwrap();
        let item = Item {
            name: "authored.T".into(),
            bytes,
            ram_base: None,
            archive: Some(archive),
            sibling_vb: None,
            parsed: None,
            archive_parser: None,
        };
        assert_eq!(scan::scan(&item.bytes, Options::default()).len(), 2);
        let hits = item_hits(&item);
        assert_eq!(hits.len(), 1);
        let hit = hit_json(&item, &hits[0]);
        assert_eq!(hit["offset"], "0x1020");
        assert_eq!(hit["entry"]["index"], 2);
        assert_eq!(hit["entry"]["offset"], "0x20");
        assert_eq!(hit["entry"]["archive_offset"], "0x1000");
        assert_eq!(hit["entry"]["size"], 4096);
        assert_eq!(hit["entry"]["format"], "dot_t");
        assert!(hit["entry"].get("name").is_none());
    }

    #[test]
    fn ordinary_assets_are_not_archives_and_explicit_modes_are_checked() {
        let bytes = tim();
        assert!(archive_index(&bytes, None, None, false).unwrap().is_none());
        assert!(archive_index(&bytes, None, Some("dot-t"), false).is_err());
        assert!(archive_index(&bytes, None, Some("unknown"), false).is_err());
        let mut archive = vec![0; 4096];
        archive[..6].copy_from_slice(&[1, 0, 1, 0, 2, 0]);
        assert!(archive_index(&archive, None, None, false).unwrap().is_some());
        assert!(archive_index(&archive, None, None, true).unwrap().is_none());
        assert!(archive_index(&archive, None, Some("none"), false).unwrap().is_none());
        assert!(archive_index(&archive, None, Some("dot-t"), true).unwrap().is_some());
    }

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
        let idx = Archive::croc_dir(&d, 16).unwrap();
        assert_eq!(idx.entries[1].name.as_deref(), Some("B.WAD"));
        assert_eq!((idx.entries[1].offset, idx.entries[1].size), (10, 6));
        assert!(Archive::croc_dir(&d, 15).is_err());
        assert_eq!(with_ext("/X/CROCFILE.1", "DIR"), "/X/CROCFILE.DIR");
    }
}
