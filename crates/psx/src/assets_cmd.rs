//! `psx assets scan|extract`: find TIM/TMD/VAB/VAG data in disc files,
//! archives or RAM dumps, and convert it to PNG / glTF / WAV.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::parser::{self, Parsers};
use psx_assets::archive::{Archive, Entry};
use psx_assets::gltf::{texture_keys, tmd_to_gltf, TextureKey};
use psx_assets::model_document::ModelDocument;
use psx_assets::scan::{self, Hit, Info, Kind, Options};
use psx_assets::upload::{Upload, UploadDocument};
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
    pub uploads: Vec<ParsedUploads>,
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

pub struct ParsedUploads {
    offset: usize,
    size: usize,
    uploads: Vec<Upload>,
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
            // Explicit upload streams have no signature and no declared pixel
            // depth. Keep them separate from standard TIM hits.
            let value: Value = parser::document(&output.bytes)?;
            if value.get("uploads").is_some() {
                let uploads = parser::document::<UploadDocument>(&output.bytes)?.decode(size).map_err(MediaError::Malformed)?;
                item.uploads.push(ParsedUploads { offset, size, uploads, document: output.bytes, provenance: output.provenance });
                continue;
            }
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

/// Host directory traversal is recursive and sorted. Do not follow symlinks:
/// a source tree may contain parent links or links to unrelated media trees.
fn host_files(path: &Path, files: &mut Vec<PathBuf>) -> R<()> {
    for entry in std::fs::read_dir(path).map_err(|e| MediaError::io(path, e))? {
        let entry = entry.map_err(|e| MediaError::io(path, e))?;
        let kind = entry.file_type().map_err(|e| MediaError::io(entry.path(), e))?;
        if kind.is_dir() {
            host_files(&entry.path(), files)?;
        } else if kind.is_file() {
            files.push(entry.path());
        }
    }
    Ok(())
}

fn host_item(source: &str, ram_base: Option<u32>, archive_mode: Option<&str>, parsers: &Parsers) -> R<Item> {
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
    finish_item(
        Item {
            name: source.to_string(),
            bytes,
            ram_base: ram_base.or(inferred_ram),
            archive,
            archive_parser: None,
            parsed: None,
            uploads: Vec::new(),
            sibling_vb: vb,
        },
        parsers,
    )
}

/// Resolve a source: `image.cue::/PATH` (a file, `/DIR/` for a directory,
/// or `*` for every file), a bare disc image (every file), or a host file/directory.
pub fn items(source: &str, ram_base: Option<u32>, archive_mode: Option<&str>, parsers: &Parsers) -> R<Vec<Item>> {
    Ok(scan_inputs(source, ram_base, archive_mode, parsers, None)?.items)
}
struct ScanInputs {
    items: Vec<Item>,
    source_files: usize,
    selected_files: usize,
    skipped: Vec<Value>,
}
fn scan_inputs(source: &str, ram_base: Option<u32>, archive_mode: Option<&str>, parsers: &Parsers, filter: Option<&str>) -> R<ScanInputs> {
    let matches = |name: &str| filter.is_none_or(|f| name.to_lowercase().contains(&f.to_lowercase()));
    let (image, pattern) = match source.split_once("::") {
        Some((i, p)) => (PathBuf::from(i), Some(p.to_string())),
        None if psx_media::disc::is_image_path(Path::new(source)) && Disc::open(source).is_ok() => (PathBuf::from(source), None),
        None => {
            let metadata = std::fs::metadata(source).map_err(|e| MediaError::io(source, e))?;
            let mut files = Vec::new();
            if metadata.is_dir() {
                host_files(Path::new(source), &mut files)?;
                files.sort();
            } else {
                files.push(PathBuf::from(source));
            }
            let source_files = files.len();
            let mut items = Vec::new();
            for path in files {
                let name = path.to_str().ok_or_else(|| MediaError::Malformed(format!("non-UTF-8 asset path: {}", path.display())))?;
                if matches(name) {
                    items.push(host_item(name, ram_base, archive_mode, parsers)?);
                }
            }
            return Ok(ScanInputs { selected_files: items.len(), items, source_files, skipped: vec![] });
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
        vec![fs.entry(&pattern)?]
    };
    let read = |path: &str| fs.find(path).filter(|e| !e.is_dir).and_then(|e| fs.read_file(&disc, e).ok());
    let mut out = Vec::new();
    let source_files = selected.len();
    let mut selected_files = 0;
    let mut skipped = Vec::new();
    for e in selected.into_iter().filter(|e| matches(&e.path)) {
        selected_files += 1;
        // Form 2 (XA/STR) files hold audio/video, not these formats.
        let bytes = match fs.read_file(&disc, e) {
            Ok(b) => b,
            Err(error) => {
                skipped.push(json!({"file":e.path,"streamed":e.is_streamed(),"reason":error.to_string()}));
                continue;
            }
        };
        let directory = (!e.path.to_ascii_uppercase().ends_with(".DIR")).then(|| read(&with_ext(&e.path, "DIR"))).flatten();
        let archive = archive_index(&bytes, directory.as_deref(), archive_mode, ram_base.is_some())?;
        let sibling_vb = e.path.to_ascii_uppercase().ends_with(".VH").then(|| read(&with_ext(&e.path, "VB"))).flatten();
        out.push(finish_item(
            Item { name: e.path.clone(), bytes, ram_base, archive, sibling_vb, archive_parser: None, parsed: None, uploads: Vec::new() },
            parsers,
        )?);
    }
    Ok(ScanInputs { items: out, source_files, selected_files, skipped })
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
    if let Some(entry) = entry_json(item, h.offset) {
        v["entry"] = entry;
    }
    if let (Some(base), Info::Tmd { .. }) = (item.ram_base, &h.info) {
        v["address"] = json!(format!("0x{:08X}", base as usize + h.offset));
    }
    v
}

fn entry_json(item: &Item, offset: usize) -> Option<Value> {
    entry_of(item, offset).map(|entry| {
        let mut provenance = serde_json::to_value(entry).unwrap();
        provenance["archive_offset"] = json!(format!("0x{:X}", entry.offset));
        // Preserve Croc's existing relative `entry.offset` and optional name.
        provenance["offset"] = json!(format!("0x{:X}", offset - entry.offset));
        provenance["format"] = json!(item.archive.as_ref().unwrap().format);
        if let Some(parser) = &item.archive_parser {
            provenance["parser"] = parser.clone();
        }
        provenance
    })
}

fn uploads_json(item: &Item, stream: &ParsedUploads) -> Value {
    // Reuse the same source/entry provenance as standard hits without presenting
    // raw words as TIM pixels or guessing which uploads are palettes.
    let mut value = json!({"kind":"vram_uploads","offset":format!("0x{:X}",stream.offset),"size":stream.size});
    if let Some(entry) = entry_json(item, stream.offset) {
        value["entry"] = entry;
    }
    value["parser"] = stream.provenance.clone();
    value["uploads"] = json!(stream
        .uploads
        .iter()
        .enumerate()
        .map(|(index, upload)| json!({
            "index":index,"rect":[upload.rect.x,upload.rect.y,upload.rect.w,upload.rect.h],"words":upload.words.len(),
            "source_offset":upload.source_offset,"source_size":upload.source_size,
            "offset":format!("0x{:X}",stream.offset+upload.source_offset),
        }))
        .collect::<Vec<_>>());
    value
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
        filter:a.filter.as_deref(),offset:a.offset,limit:a.limit,item_offset:a.item_offset,
    })
}

/// `assets extract`.
pub fn extract_assets(a: &crate::commands::AssetsExtractArgs, ctx: &crate::commands::Ctx) -> R<Value> {
    crate::page::validate(a.limit).map_err(MediaError::Malformed)?;
    let parsers = a.parsers.build(ctx, a.archive.as_deref())?;
    let texture_parser = parser::Spec::new(a.texture_parser.as_ref(), &a.texture_parser_args, a.parsers.parser_timeout)?;
    if texture_parser.is_some() && a.textures.is_empty() {
        return Err(MediaError::Malformed("--texture-parser requires at least one --textures source".into()));
    }
    let full = extract_cmd(ExtractArgs {
        source: &a.source,
        out: &a.out,
        kinds: &kinds(&a.kinds)?,
        offset: hex(&a.offset)?.map(|o| o as usize),
        textures: &a.textures,
        ram_base: hex(&a.ram_base)?,
        archive: a.archive.as_deref(),
        entry: a.entry,
        parsers: &parsers,
        texture_parser: texture_parser.as_ref(),
    })?;
    extraction_response(full, &a.out, a.brief, a.report_offset, a.limit)
}

/// Persist complete conversion evidence before trimming any response lists.
fn extraction_response(mut full: Value, out: &Path, brief: bool, offset: usize, limit: usize) -> R<Value> {
    let mut kinds = BTreeMap::<String, usize>::new();
    let mut outputs = 0;
    let mut warnings = 0;
    let mut skipped_primitives = 0u64;
    for key in ["assets", "entries"] {
        for row in full.get_mut(key).and_then(Value::as_array_mut).into_iter().flatten() {
            let mut additional = Vec::new();
            for page in row["details"]["texture_pages"].as_array().into_iter().flatten() {
                if let Some(path) = page["png"].as_str() { additional.push(json!({"path":path,"role":"texture_page"})); }
            }
            for upload in row["uploads"].as_array().into_iter().flatten() {
                if let Some(path) = upload["path"].as_str() { additional.push(json!({"path":path,"role":"vram_words"})); }
            }
            if let Some(files) = row.get_mut("outputs").and_then(Value::as_array_mut) {
                for extra in additional {
                    if !files.iter().any(|f| f["path"] == extra["path"]) { files.push(extra); }
                }
                for file in files {
                    if let Some(path) = file["path"].as_str() {
                        let path = PathBuf::from(path);
                        file["sha256"] = json!(psx_media::disc::sha256_file(&path)?);
                        file["bytes"] = json!(std::fs::metadata(&path).map_err(|e| MediaError::io(&path, e))?.len());
                    }
                }
            }
            *kinds.entry(row["kind"].as_str().unwrap_or("archive_entry").into()).or_default() += 1;
            outputs += row["outputs"].as_array().map_or(0, Vec::len);
            warnings += usize::from(row["details"]["warning"].is_string());
            skipped_primitives += row["details"]["skipped_primitives"].as_u64().unwrap_or(0);
        }
    }
    full["summary"] = json!({"records":full["extracted"],"output_files":outputs,"by_kind":kinds,"warnings":warnings,
        "skipped_primitives":skipped_primitives});
    std::fs::create_dir_all(out).map_err(|e| MediaError::io(out, e))?;
    let bytes = serde_json::to_vec_pretty(&full).unwrap();
    let hash = psx_media::disc::sha256_bytes(&bytes);
    let manifest = std::path::absolute(out).map_err(|e| MediaError::io(out, e))?
        .join(format!("extraction-{hash}.json"));
    if manifest.exists() {
        if std::fs::read(&manifest).map_err(|e| MediaError::io(&manifest, e))? != bytes {
            return Err(MediaError::Malformed(format!("extraction manifest changed: {}", manifest.display())));
        }
    } else {
        let staged = out.join(format!(".extraction-{}{}.tmp", std::process::id(), crate::instance_suffix()));
        write(&staged, &bytes)?;
        if let Err(e) = std::fs::rename(&staged, &manifest) {
            let _ = std::fs::remove_file(&staged);
            if std::fs::read(&manifest).ok().as_deref() != Some(bytes.as_slice()) {
                return Err(MediaError::io(&manifest, e));
            }
        }
    }
    let mut response = full;
    for key in ["assets", "entries", "texture_sources"] {
        if let Some(rows) = response[key].as_array() {
            let total = rows.len();
            let mut page: Vec<_> = rows.iter().skip(offset).take(if brief { 0 } else { limit }).cloned().collect();
            for row in &mut page {
                for field in ["outputs", "uploads", "hits", "upload_streams"] {
                    if let Some(values) = row[field].as_array() {
                        let total = values.len();
                        row[format!("{field}_page")] = crate::page::metadata(total, 0, total.min(limit), limit);
                        row[field].as_array_mut().unwrap().truncate(limit);
                    }
                }
                if let Some(pages) = row.get_mut("details").and_then(|d| d.get_mut("texture_pages")).and_then(Value::as_array_mut) {
                    let total = pages.len();
                    pages.truncate(limit);
                    row["details"]["texture_pages_page"] = crate::page::metadata(total, 0, total.min(limit), limit);
                }
            }
            response[format!("{key}_page")] = crate::page::metadata(total, offset, page.len(), limit);
            response[key] = json!(page);
        }
    }
    response["manifest"] = crate::artifact::descriptor(&manifest).map_err(|e| MediaError::io(&manifest, e))?;
    let list = if response["entries_page"]["total"].as_u64().unwrap_or(0) > 0 { "entries" } else { "assets" };
    response["next_call"] = json!({"tool":"artifact_read","arguments":{"path":manifest,"pointer":format!("/{list}"),"offset":offset,"limit":limit}});
    response["brief"] = json!(brief);
    Ok(response)
}

fn hex(s: &Option<String>) -> R<Option<u32>> {
    s.as_deref()
        .map(|v| {
            psx_media::memmap::parse_hex(v)
                .ok_or_else(|| MediaError::Malformed(format!("bad hex value {v}")))
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
    pub filter: Option<&'a str>,
    pub offset: usize,
    pub limit: usize,
    pub item_offset: usize,
}

/// Returns the JSON report, or a text table when `json` is false.
pub fn scan_cmd(a: ScanArgs) -> R<Result<Value, String>> {
    crate::page::validate(a.limit).map_err(MediaError::Malformed)?;
    let ScanInputs{items,source_files,selected_files,skipped}=scan_inputs(a.source,a.ram_base,a.archive,a.parsers,a.filter)?;
    let mut files = Vec::new();
    let mut totals: BTreeMap<Kind, usize> = BTreeMap::new();
    let (mut upload_streams, mut uploads, mut upload_words) = (0, 0, 0);
    for item in &items {
        let hits: Vec<Hit> = item_hits(item).into_iter().filter(|h| a.kinds.is_empty() || a.kinds.contains(&h.kind)).collect();
        let streams: Vec<_> = item.uploads.iter().filter(|_| a.kinds.is_empty()).collect();
        if hits.is_empty() && streams.is_empty() && item.archive.is_none() {
            continue;
        }
        for h in &hits {
            *totals.entry(h.kind).or_default() += 1;
        }
        let mut file =
            json!({"file": item.name, "bytes": item.bytes.len(), "hits": hits.iter().map(|h| hit_json(item, h)).collect::<Vec<_>>()});
        if !streams.is_empty() {
            upload_streams += streams.len();
            file["upload_streams"] = json!(streams.iter().map(|stream| uploads_json(item, stream)).collect::<Vec<_>>());
            for stream in &streams {
                uploads += stream.uploads.len();
                upload_words += stream.uploads.iter().map(|u| u.words.len()).sum::<usize>();
            }
        }
        if let Some(archive) = &item.archive {
            file["archive"] = json!(archive);
            if let Some(parser) = &item.archive_parser {
                file["archive"]["parser"] = parser.clone();
            }
        }
        files.push(file);
    }
    let total = files.len();
    let mut files: Vec<_> = files.into_iter().skip(a.offset).take(a.limit).collect();
    for file in &mut files {
        for (field, page_field) in [("hits", "hits_page"), ("upload_streams", "upload_streams_page")] {
            if let Some(rows) = file[field].as_array() {
                let page_rows: Vec<_> = rows.iter().skip(a.item_offset).take(a.limit).cloned().collect();
                file[page_field] = crate::page::metadata(rows.len(), a.item_offset, page_rows.len(), a.limit);
                file[field] = json!(page_rows);
            }
        }
        if let Some(rows) = file["archive"]["entries"].as_array() {
            let page_rows: Vec<_> = rows.iter().skip(a.item_offset).take(a.limit).cloned().collect();
            file["archive"]["entries_page"] = crate::page::metadata(rows.len(), a.item_offset, page_rows.len(), a.limit);
            file["archive"]["entries"] = json!(page_rows);
        }
    }
    let skipped_rows:Vec<_>=skipped.iter().skip(a.offset).take(a.limit).collect();
    let mut result = json!({"source": a.source, "files_scanned": items.len(), "totals": totals.iter().map(|(k, n)| (k.ext().to_string(), json!(n))).collect::<serde_json::Map<_, _>>(),
        "page":crate::page::metadata(total,a.offset,files.len(),a.limit),
        "coverage":{"scope":"validated standard-format signatures plus selected archive/parser extensions","source_files":source_files,"selected_files":selected_files,"files_inspected":items.len(),"complete":skipped.is_empty(),
            "skipped_files":skipped_rows,"skipped_page":crate::page::metadata(skipped.len(),a.offset,skipped_rows.len(),a.limit),
            "standard_kinds":if a.kinds.is_empty(){vec!["tim","tmd","vab","vag"]}else{a.kinds.iter().map(|k|k.ext()).collect()},
            "note":"No recognized hit is not evidence that custom models, textures or other assets are absent."},"files": files});
    if upload_streams != 0 {
        result["upload_totals"] = json!({"streams":upload_streams,"uploads":uploads,"words":upload_words});
    }
    if !a.json {
        let mut text = String::new();
        for file in result["files"].as_array().unwrap() {
            text.push_str(&format!(
                "{}: {} hits, {} uploads\n",
                file["file"].as_str().unwrap_or(""),
                file["hits_page"]["total"],
                file["upload_streams_page"]["total"].as_u64().unwrap_or(0)
            ));
            for hit in file["hits"].as_array().unwrap().iter().take(3) {
                text.push_str(&format!(
                    "  {} {} ({} bytes)\n",
                    hit["kind"].as_str().unwrap_or(""),
                    hit["offset"].as_str().unwrap_or(""),
                    hit["size"]
                ));
            }
            if file["hits"].as_array().unwrap().len()>3{text.push_str("  More hits: use json=true with item_offset/limit.\n");}
        }
        text.push_str(&format!("{} files inspected; page {}\nCoverage: standard signatures and configured parser extensions; custom formats may remain unrecognized.\n",items.len(),result["page"]));
        result.as_object_mut().unwrap().remove("files");
        result["text"] = json!(text);
    }
    Ok(Ok(result))
}

pub struct ExtractArgs<'a> {
    pub source: &'a str,
    pub out: &'a Path,
    pub kinds: &'a [Kind],
    /// Only the hit at this offset.
    pub offset: Option<usize>,
    /// Extra texture sources loaded before each item's own textures.
    pub textures: &'a [String],
    pub ram_base: Option<u32>,
    pub archive: Option<&'a str>,
    /// Raw entry extraction, without interpreting its payload.
    pub entry: Option<usize>,
    pub parsers: &'a Parsers,
    pub texture_parser: Option<&'a parser::Spec>,
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

/// Apply source entries in index order (byte order for a flat file), and each
/// upload document in its returned order. TIMs and raw streams may be interleaved.
fn load_item_textures(vram: &mut Vram, item: &Item) -> (usize, usize) {
    enum Source<'a> {
        Tim(Hit),
        Uploads(&'a ParsedUploads),
    }
    let order = |offset| (entry_of(item, offset).map_or(0, |e| e.index), offset);
    let mut sources: Vec<_> =
        item_hits(item).into_iter().filter(|h| h.kind == Kind::Tim).map(|h| (order(h.offset), Source::Tim(h))).collect();
    sources.extend(item.uploads.iter().map(|s| (order(s.offset), Source::Uploads(s))));
    sources.sort_by_key(|(order, _)| *order);
    let (mut tims, mut uploads) = (0, 0);
    for (_, source) in sources {
        match source {
            Source::Tim(hit) => {
                let (bytes, offset, _) = decode_context(item, hit.offset);
                if let Ok(tim) = Tim::parse(&bytes[offset..]) {
                    vram.load_tim(&tim);
                    tims += 1;
                }
            }
            Source::Uploads(stream) => {
                for upload in &stream.uploads {
                    upload.apply(vram);
                    uploads += 1;
                }
            }
        }
    }
    (tims, uploads)
}

pub fn extract_cmd(a: ExtractArgs) -> R<Value> {
    if a.entry.is_some()
        && (a.offset.is_some()
            || !a.kinds.is_empty()
            || !a.textures.is_empty()
            || a.parsers.model.is_some()
            || a.parsers.asset.is_some()
            || a.texture_parser.is_some())
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
    let mut texture_uploads = 0;
    let mut texture_sources = Vec::new();
    let texture_parsers = Parsers { asset: a.texture_parser.cloned(), cancel: a.parsers.cancel.clone(), ..Default::default() };
    for t in a.textures {
        for it in self::items(t, None, None, &texture_parsers)? {
            let (tims, uploads) = load_item_textures(&mut base_vram, &it);
            texture_tims += tims;
            texture_uploads += uploads;
            if a.texture_parser.is_some() {
                texture_sources.push(json!({"file":it.name,
                    "hits":item_hits(&it).iter().map(|h|hit_json(&it,h)).collect::<Vec<_>>(),
                    "upload_streams":it.uploads.iter().map(|s|uploads_json(&it,s)).collect::<Vec<_>>()
                }));
            }
        }
    }
    let mut report = Vec::new();
    let want = |k: Kind| a.kinds.is_empty() || a.kinds.contains(&k);
    for item in &items {
        let hits: Vec<Hit> = item_hits(item).into_iter().filter(|h| want(h.kind) && a.offset.is_none_or(|o| o == h.offset)).collect();
        let streams: Vec<_> = item.uploads.iter().filter(|s| a.kinds.is_empty() && a.offset.is_none_or(|o| o == s.offset)).collect();
        if hits.is_empty() && streams.is_empty() {
            continue;
        }
        let dir = a.out.join(safe_name(&item.name));
        std::fs::create_dir_all(&dir).map_err(|e| MediaError::io(&dir, e))?;
        for stream in streams {
            let stem = format!("{:08X}", stream.offset);
            let source = dir.join(format!("{stem}.source.bin"));
            let document = dir.join(format!("{stem}.parser.json"));
            write(&source, &item.bytes[stream.offset..stream.offset + stream.size])?;
            write(&document, &stream.document)?;
            let mut record = uploads_json(item, stream);
            record["file"] = json!(item.name);
            let uploads = record["uploads"].as_array_mut().unwrap();
            for (index, upload) in stream.uploads.iter().enumerate() {
                let path = dir.join(format!("{stem}_upload_{index:04}.words.bin"));
                write(&path, &upload.bytes())?;
                uploads[index]["path"] = json!(path);
            }
            let manifest = dir.join(format!("{stem}.uploads.json"));
            record["outputs"] = json!([{"path":source,"role":"original"},{"path":document,"role":"parser_document"},{"path":manifest,"role":"upload_manifest"}]);
            write(&manifest, serde_json::to_string_pretty(&record).unwrap().as_bytes())?;
            report.push(record);
        }
        // VRAM for this item's models: the --textures TIMs, then this item's own.
        let mut vram = base_vram.clone();
        let (own_tims, own_uploads) = if hits.iter().any(|h| h.kind == Kind::Tmd) { load_item_textures(&mut vram, item) } else { (0, 0) };
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
                        "polygons": t.polygon_count(),
                        "skipped_primitives": t.objects.iter().map(|o| u64::from(o.skipped)).sum::<u64>(),
                        "vram_tims": {"from_textures": texture_tims, "from_this_file": own_tims},
                        "texture_pages": pages.iter().map(|(k, (f, c))| json!({
                            "page": [k.page.x, k.page.y], "bits": k.page.bits, "clut": k.clut.map(|c| [c.0, c.1]),
                            "png": (*c > 0.0).then(|| dir.join(f)), "vram_coverage": (c * 1000.0).round() / 1000.0,
                        })).collect::<Vec<_>>(),
                    });
                    if extra["skipped_primitives"].as_u64().unwrap_or(0) > 0 {
                        extra["warning"] = json!("TMD conversion omitted primitives with unsupported types or packet layouts; geometry is incomplete. For preprocessed model data, use --model-parser or normalize it with --asset-parser; see psx guide --topic assets.");
                    }
                    if texture_uploads + own_uploads > 0 {
                        extra["vram_uploads"] = json!({"from_textures":texture_uploads,"from_this_file":own_uploads});
                    }
                }
                Kind::Vag => {
                    let v = Vag::parse(bytes).map_err(MediaError::Malformed)?;
                    let d = v.decode(bytes);
                    let p = dir.join(format!("{stem}.wav"));
                    write(&p, &wav_mono(&d.pcm, v.rate))?;
                    let header = psx_assets::vab::VAG_HEADER;
                    let data = &bytes[header..(header + v.data_size).min(bytes.len())];
                    outputs.push(json!({"path": p, "wav": wav_stats(&d.pcm, v.rate), "loop": d.looped,
                        "adpcm_sha256": adpcm::sample_sha256(data, 0)}));
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
                                outputs.push(json!({"path": p, "waveform": i + 1, "wav": wav_stats(&d.pcm, 44_100), "loop": d.looped,
                                    "adpcm_sha256": adpcm::sample_sha256(data, 0)}));
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
    let mut result = json!({"source": a.source, "out": a.out, "extracted": report.len(), "assets": report});
    if !texture_sources.is_empty() {
        result["texture_sources"] = json!(texture_sources);
    }
    Ok(result)
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
            uploads: Vec::new(),
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
    fn tims_and_raw_uploads_follow_archive_entry_order_even_when_offsets_are_unsorted() {
        let tim = tim();
        let hit = scan::scan(&tim, Options::default()).remove(0);
        let mut bytes = vec![0; 129];
        bytes[..tim.len()].copy_from_slice(&tim);
        let archive = Archive::external(
            vec![Entry { index: 0, name: None, offset: 128, size: 1 }, Entry { index: 1, name: None, offset: 0, size: tim.len() }],
            bytes.len(),
        )
        .unwrap();
        let item = Item {
            name: "mixed.dat".into(),
            bytes,
            ram_base: None,
            archive: Some(archive),
            archive_parser: None,
            sibling_vb: None,
            parsed: Some(vec![ParsedAsset { hit, model: None, normalized: None, document: vec![], provenance: Value::Null }]),
            uploads: vec![ParsedUploads {
                offset: 128,
                size: 1,
                document: vec![],
                provenance: Value::Null,
                uploads: vec![Upload {
                    source_offset: 0,
                    source_size: 1,
                    rect: psx_assets::tim::VramRect { x: 0, y: 0, w: 1, h: 1 },
                    words: vec![0x3e0],
                }],
            }],
        };
        let mut vram = Vram::default();
        assert_eq!(load_item_textures(&mut vram, &item), (1, 1));
        assert_eq!(vram.data[0], 31, "the later TIM entry must overwrite the raw upload");
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
