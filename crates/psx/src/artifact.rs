//! Bounded local artifact inspection, shared by CLI and MCP. No process-wide cwd changes.
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::Error;
use crate::commands::{Ctx, Op, Outcome};
use base64::Engine;
use clap::{Args, ValueEnum};
use psx_media::disc::sha256_bytes;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const SCAN_BYTES: u64 = 4 * 1024 * 1024;
const JSON_BYTES: u64 = 16 * 1024 * 1024;
const RECORD_BYTES: usize = 64 * 1024;
const RESPONSE_BYTES: usize = 96 * 1024;
const IMAGE_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum, Deserialize, Serialize, JsonSchema, Default)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    #[default]
    Auto,
    Json,
    Jsonl,
    Csv,
    Text,
    Hex,
    Image,
}

#[derive(Args, Deserialize, JsonSchema)]
pub struct ReadArgs {
    /// Artifact file or directory (e.g. a run_dir); absolute paths also work.
    pub path: PathBuf,
    /// Auto uses the filename extension. Image supports PNG; binary files use hex.
    #[arg(long, value_enum, default_value = "auto")]
    #[serde(default)]
    pub format: Format,
    /// RFC 6901 JSON pointer into a JSON document, e.g. /files or /functions/items.
    #[arg(long)]
    pub pointer: Option<String>,
    /// Initial row offset (JSON array, directory, or stream records); bytes in hex mode.
    #[arg(long, default_value_t = 0)]
    #[serde(default)]
    pub offset: usize,
    /// Maximum rows, or bytes in hex mode (1..=1000). Responses also have a byte cap.
    #[arg(long, default_value_t=crate::page::default_limit())]
    #[serde(default = "crate::page::default_limit")]
    pub limit: usize,
    /// Opaque continuation from page.next_cursor. Preserves source/query identity; omit offset.
    #[arg(long)]
    pub cursor: Option<String>,
    /// Keep JSONL/CSV records at or after this frame (inclusive).
    #[arg(long)]
    pub frame_from: Option<u64>,
    /// Keep JSONL/CSV records at or before this frame (inclusive).
    #[arg(long)]
    pub frame_to: Option<u64>,
    /// Frame field used by filters: frame (absolute) or run_frame (relative).
    #[arg(long, default_value = "frame")]
    #[serde(default = "frame_field")]
    pub frame_field: String,
    /// Keep JSONL/CSV records with this PC (hex, compared numerically to pc/pc_hex).
    #[arg(long)]
    pub pc: Option<String>,
    /// Case-sensitive substring filter on a serialized record or text line.
    #[arg(long)]
    pub contains: Option<String>,
    /// Return only these JSONL/CSV fields; repeatable. Filters run before projection.
    #[arg(long = "column")]
    #[serde(default)]
    pub columns: Vec<String>,
}
fn frame_field() -> String {
    "frame".into()
}
type R<T> = crate::Result<T>;
fn hex(s: &str) -> Option<u64> {
    u64::from_str_radix(s.trim_start_matches("0x").trim_start_matches("0X"), 16).ok()
}
fn detected(path: &Path) -> Format {
    match path.extension().and_then(|s| s.to_str()).unwrap_or("").to_ascii_lowercase().as_str() {
        "json" => Format::Json,
        "jsonl" | "ndjson" => Format::Jsonl,
        "csv" => Format::Csv,
        "png" => Format::Image,
        "bin" | "rom" | "mcd" | "raw" | "exe" | "ps-exe" => Format::Hex,
        _ => Format::Text,
    }
}
pub fn descriptor(path: &Path) -> std::io::Result<Value> {
    let m = fs::metadata(path)?;
    let format = detected(path);
    Ok(json!({"path":path,"bytes":m.len(),"directory":m.is_dir(),"format":format,
        "read":{"tool":"artifact_read","arguments":{"path":path}}}))
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    source: String,
    query: String,
    position: u64,
    record: u64,
    #[serde(default)]
    skip_until: u64,
}
fn encode(c: Cursor) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(serde_json::to_vec(&c).unwrap())
}
fn decode(s: &str) -> R<Cursor> {
    if s.len() > 4096 {
        return Err(Error::invalid("cursor exceeds 4096 bytes"));
    }
    let b = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(s).map_err(Error::invalid)?;
    serde_json::from_slice(&b).map_err(Error::invalid)
}

impl ReadArgs {
    fn matches(&self, v: &Value, pc: Option<u64>) -> bool {
        let number = |v: &Value| v.as_u64().or_else(|| v.as_str()?.parse().ok());
        if self.frame_from.is_some() || self.frame_to.is_some() {
            let Some(f) = number(&v[&self.frame_field]) else { return false };
            if self.frame_from.is_some_and(|n| f < n) || self.frame_to.is_some_and(|n| f > n) {
                return false;
            }
        }
        if let Some(pc) = pc {
            let found = v["pc"]
                .as_u64()
                .or_else(|| v["pc_hex"].as_str().and_then(hex))
                .or_else(|| v["pc"].as_str().and_then(|s| s.parse().ok().or_else(|| hex(s))));
            if found != Some(pc) {
                return false;
            }
        }
        self.contains.as_ref().is_none_or(|s| v.as_str().map_or_else(|| v.to_string().contains(s), |v| v.contains(s)))
    }
    fn project(&self, v: Value) -> R<Value> {
        if self.columns.is_empty() {
            return Ok(v);
        }
        let o = v.as_object().ok_or_else(|| Error::invalid("column projection requires object records"))?;
        for key in &self.columns {
            if !o.contains_key(key) {
                return Err(Error::invalid(format!("unknown column {key:?}")));
            }
        }
        Ok(Value::Object(self.columns.iter().map(|k| (k.clone(), o[k].clone())).collect()))
    }
    fn execute(&self, ctx: &Ctx) -> R<Value> {
        crate::page::validate(self.limit).map_err(Error::invalid)?;
        if self.frame_from.zip(self.frame_to).is_some_and(|(a, b)| a > b) {
            return Err(Error::invalid("frame_from exceeds frame_to"));
        }
        if !matches!(self.frame_field.as_str(), "frame" | "run_frame") {
            return Err(Error::invalid("frame_field must be frame or run_frame"));
        }
        if self.cursor.is_some() && self.offset != 0 {
            return Err(Error::invalid("cursor cannot combine with offset"));
        }
        let p = if self.path.is_absolute() { self.path.clone() } else { ctx.root.join(&self.path) };
        let p = p.canonicalize().map_err(|e| Error::io(&p, e))?;
        let m = fs::metadata(&p).map_err(|e| Error::io(&p, e))?;
        if !m.is_file() && !m.is_dir() {
            return Err(Error::invalid("artifact must be a regular file or directory"));
        }
        let format = if self.format == Format::Auto { detected(&p) } else { self.format };
        let row_filters =
            self.frame_from.is_some() || self.frame_to.is_some() || self.pc.is_some() || !self.columns.is_empty();
        if (row_filters && !matches!(format, Format::Jsonl | Format::Csv))
            || (self.contains.is_some() && !matches!(format, Format::Jsonl | Format::Csv | Format::Text))
        {
            return Err(Error::invalid("record filters/projection require JSONL/CSV; contains also supports text"));
        }
        if self.pointer.is_some() && format != Format::Json {
            return Err(Error::invalid("pointer requires JSON format"));
        }
        let pc =
            self.pc.as_deref().map(|s| hex(s).ok_or_else(|| Error::invalid("pc must be a hex address"))).transpose()?;
        let source = sha256_bytes(
            &serde_json::to_vec(&json!({"path":p,"bytes":m.len(),
            "modified_ns":m.modified().ok().and_then(|t|t.duration_since(UNIX_EPOCH).ok()).map(|d|d.as_nanos().to_string())}))
            .unwrap(),
        );
        let query = sha256_bytes(
            &serde_json::to_vec(&json!({"format":format,"pointer":self.pointer,"frame_from":self.frame_from,
            "frame_to":self.frame_to,"frame_field":self.frame_field,"pc":pc,"contains":self.contains,"columns":self.columns}))
            .unwrap(),
        );
        let cursor = self.cursor.as_deref().map(decode).transpose()?;
        if cursor.as_ref().is_some_and(|c| c.source != source || c.query != query) {
            return Err(Error::new("stale_cursor", "Source metadata or query changed; restart without cursor."));
        }
        let mut out = json!({"artifact":descriptor(&p)?,"source_id":source,"format":format});
        if m.is_dir() {
            if self.format != Format::Auto || self.pointer.is_some() || row_filters || self.contains.is_some() {
                return Err(Error::invalid("directories accept pagination only"));
            }
            let mut paths = Vec::new();
            for e in fs::read_dir(&p).map_err(|e| Error::io(&p, e))? {
                paths.push(e?.path());
                if paths.len() > 10000 {
                    return Err(Error::new(
                        "limit_exceeded",
                        "Directory exceeds 10000 entries; inspect a subdirectory.",
                    ));
                }
            }
            paths.sort();
            let start = cursor.as_ref().map_or(self.offset as u64, |c| c.position) as usize;
            let mut rows = Vec::new();
            let mut bytes = 0;
            for p in paths.iter().skip(start).take(self.limit) {
                let row = descriptor(p)?;
                let len = row.to_string().len();
                if bytes + len > RESPONSE_BYTES {
                    break;
                }
                bytes += len;
                rows.push(row);
            }
            out["page"] = crate::page::metadata(paths.len(), start, rows.len(), self.limit);
            out["items"] = json!(rows);
            out["format"] = json!("directory");
        } else if format == Format::Json {
            if m.len() > JSON_BYTES {
                return Err(Error::new(
                    "limit_exceeded",
                    "JSON exceeds 16 MiB; use text/hex mode or a smaller artifact.",
                ));
            }
            let b = read_bounded(&p, JSON_BYTES)?;
            let value: Value = serde_json::from_slice(&b).map_err(|e| Error::new("invalid_artifact", e))?;
            let value = match &self.pointer {
                Some(ptr) => value.pointer(ptr).ok_or_else(|| Error::invalid("JSON pointer does not exist"))?,
                None => &value,
            };
            let start = cursor.as_ref().map_or(self.offset as u64, |c| c.position) as usize;
            if let Some(rows) = value.as_array() {
                let mut selected = Vec::new();
                let mut bytes = 0;
                for row in rows.iter().skip(start).take(self.limit) {
                    let len = row.to_string().len();
                    if bytes + len > RESPONSE_BYTES {
                        if selected.is_empty() {
                            return Err(Error::new(
                                "limit_exceeded",
                                "One JSON row exceeds 96 KiB; select a narrower pointer.",
                            ));
                        }
                        break;
                    }
                    bytes += len;
                    selected.push(row.clone());
                }
                out["page"] = crate::page::metadata(rows.len(), start, selected.len(), self.limit);
                out["items"] = json!(selected);
            } else {
                if start != 0 {
                    return Err(Error::invalid("offset requires a JSON array"));
                }
                if value.to_string().len() > RESPONSE_BYTES {
                    return Err(Error::new("limit_exceeded", "JSON value exceeds 96 KiB; select a narrower pointer."));
                }
                out["value"] = value.clone();
                out["page"] = crate::page::metadata(1, 0, 1, self.limit);
            }
            out["artifact"]["sha256"] = json!(sha256_bytes(&b));
            out["coverage"] = json!({"bytes_inspected":b.len(),"source_complete":true});
        } else if format == Format::Hex {
            let start = cursor.as_ref().map_or(self.offset as u64, |c| c.position);
            let mut f = File::open(&p).map_err(|e| Error::io(&p, e))?;
            f.seek(SeekFrom::Start(start))?;
            let mut b = Vec::new();
            f.take(self.limit as u64).read_to_end(&mut b)?;
            out["hex"] = json!(b.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" "));
            let end = start.saturating_add(b.len() as u64);
            out["page"] = json!({"total":m.len(),"offset":start,"returned":b.len(),"limit":self.limit,"unit":"bytes",
                "complete":start==0 && end>=m.len(),"truncated":end<m.len(),"next_offset":(end<m.len()).then_some(end)});
        } else if format == Format::Image {
            if self.offset != 0 || self.cursor.is_some() {
                return Err(Error::invalid("PNG images do not paginate"));
            }
            let b = read_bounded(&p, IMAGE_BYTES)?;
            if !b.starts_with(b"\x89PNG\r\n\x1a\n") {
                return Err(Error::new("invalid_artifact", "image mode requires a PNG"));
            }
            let reader = png::Decoder::new(std::io::Cursor::new(&b))
                .read_info()
                .map_err(|e| Error::new("invalid_artifact", e))?;
            let info = reader.info();
            out["image"] = json!({"path":p,"mime_type":"image/png","width":info.width,"height":info.height});
            out["artifact"]["sha256"] = json!(sha256_bytes(&b));
            out["page"] = crate::page::metadata(1, 0, 1, self.limit);
        } else {
            out = self.stream(&p, m.len(), format, source.clone(), query.clone(), cursor, pc, out, ctx)?;
        }
        if out["page"]["truncated"] == true && out["page"].get("next_cursor").is_none() {
            let pos = out["page"]["next_offset"].as_u64().unwrap();
            out["page"]["next_cursor"] =
                json!(encode(Cursor { source, query, position: pos, record: pos, skip_until: 0 }));
        }
        let after = fs::metadata(&p).map_err(|e| Error::io(&p, e))?;
        if after.len() != m.len() || after.modified().ok() != m.modified().ok() {
            return Err(Error::new(
                "source_changed",
                "Artifact changed during inspection; retry against a stable file.",
            ));
        }
        if serde_json::to_string_pretty(&out).unwrap().len() > 192 * 1024 {
            return Err(Error::new(
                "limit_exceeded",
                "Formatted response exceeds 192 KiB; reduce limit, project columns or select a narrower JSON pointer.",
            ));
        }
        Ok(out)
    }

    #[allow(clippy::too_many_arguments)]
    fn stream(
        &self,
        p: &Path,
        size: u64,
        format: Format,
        source: String,
        query: String,
        cursor: Option<Cursor>,
        pc: Option<u64>,
        mut out: Value,
        ctx: &Ctx,
    ) -> R<Value> {
        let mut f = BufReader::new(File::open(p).map_err(|e| Error::io(p, e))?);
        let header = if format == Format::Csv {
            let (n, header) = record(&mut f, format)?;
            let fields =
                csv(&header)?.ok_or_else(|| Error::new("invalid_artifact", "CSV header has an unclosed quote"))?;
            let unique: std::collections::BTreeSet<_> = fields.iter().collect();
            if n == 0 || fields.iter().any(String::is_empty) || unique.len() != fields.len() {
                return Err(Error::new("invalid_artifact", "CSV needs a nonempty header with unique columns"));
            }
            Some(fields)
        } else {
            None
        };
        let header_bytes = f.stream_position()?;
        let initial = cursor.as_ref().map_or(f.stream_position()?, |c| c.position);
        if initial > size {
            return Err(Error::invalid("cursor is outside the source"));
        }
        f.seek(SeekFrom::Start(initial))?;
        let mut index = cursor.as_ref().map_or(0, |c| c.record);
        let skip_until = cursor.as_ref().map_or(self.offset as u64, |c| c.skip_until);
        let start = index;
        let mut bytes = header_bytes;
        let mut scanned = 0;
        let mut rows = Vec::new();
        let mut returned_bytes = 0;
        let mut position = initial;
        let mut matches = 0;
        while position < size && bytes < SCAN_BYTES && rows.len() < self.limit {
            if ctx.cancel.as_ref().is_some_and(|t| t.is_cancelled()) {
                return Err(Error::new("cancelled", "Artifact inspection cancelled"));
            }
            let before = position;
            let (n, line) = record(&mut f, format)?;
            if n == 0 {
                break;
            }
            bytes += n;
            position += n;
            scanned += 1;
            let row = match format {
                Format::Jsonl => serde_json::from_slice(&line)
                    .map_err(|e| Error::new("invalid_artifact", format!("JSONL record {index}: {e}")))?,
                Format::Csv => {
                    let fields =
                        csv(&line)?.ok_or_else(|| Error::new("invalid_artifact", "CSV has an unclosed quote"))?;
                    let header = header.as_ref().unwrap();
                    if fields.len() != header.len() {
                        return Err(Error::new(
                            "invalid_artifact",
                            format!("CSV record {index}: expected {} columns, found {}", header.len(), fields.len()),
                        ));
                    }
                    Value::Object(header.iter().cloned().zip(fields.into_iter().map(Value::String)).collect())
                }
                _ => json!(
                    std::str::from_utf8(&line)
                        .map_err(|_| Error::new("invalid_artifact", "Text is not UTF-8; use hex mode"))?
                        .trim_end_matches(['\r', '\n'])
                ),
            };
            if index >= skip_until && self.matches(&row, pc) {
                let row = self.project(row)?;
                let len = row.to_string().len();
                if returned_bytes + len > RESPONSE_BYTES {
                    if rows.is_empty() {
                        return Err(Error::new(
                            "limit_exceeded",
                            "Record exceeds response budget; project fewer columns or use hex mode",
                        ));
                    }
                    position = before;
                    break;
                }
                returned_bytes += len;
                rows.push(row);
                matches += 1;
            }
            index += 1;
        }
        let more = position < size;
        out["items"] = json!(rows);
        out["columns"] = json!(header);
        out["page"] = json!({"total":if cursor.is_none() && self.offset==0 {(!more).then_some(matches)}else{None},
            "offset":if cursor.is_some(){start}else{self.offset as u64},"returned":rows.len(),"limit":self.limit,"truncated":more,"complete":cursor.is_none() && self.offset==0 && !more,
            "next_offset":more.then_some(index),"next_cursor":more.then(||encode(Cursor{source,query,position,record:index,skip_until}))});
        out["coverage"] = json!({"source_complete":!more,"start_byte":initial,"end_byte":position,"bytes_inspected":bytes,
            "records_inspected":scanned,"scan_byte_budget":SCAN_BYTES,"record_byte_limit":RECORD_BYTES,
            "note":"Stream totals are unknown until the entire query is inspected from its beginning; use next_cursor to continue without rescanning."});
        Ok(out)
    }
}

fn read_bounded(p: &Path, limit: u64) -> R<Vec<u8>> {
    let mut b = Vec::new();
    File::open(p).map_err(|e| Error::io(p, e))?.take(limit + 1).read_to_end(&mut b)?;
    if b.len() as u64 > limit {
        return Err(Error::new("limit_exceeded", format!("Artifact exceeds the {limit}-byte budget for this format")));
    }
    Ok(b)
}

fn record(reader: &mut BufReader<File>, format: Format) -> R<(u64, Vec<u8>)> {
    let mut line = Vec::new();
    loop {
        let remaining = RECORD_BYTES + 1 - line.len();
        let n = reader.take(remaining as u64).read_until(b'\n', &mut line)?;
        if line.len() > RECORD_BYTES {
            return Err(Error::new("limit_exceeded", "Record exceeds 64 KiB; use hex mode for byte inspection"));
        }
        if format != Format::Csv || csv(&line)?.is_some() || n == 0 {
            break;
        }
    }
    Ok((line.len() as u64, line))
}

/// RFC 4180 fields, including escaped quotes and quoted newlines. None means incomplete quoted record.
fn csv(bytes: &[u8]) -> R<Option<Vec<String>>> {
    let s = std::str::from_utf8(bytes).map_err(|_| Error::new("invalid_artifact", "CSV is not UTF-8"))?;
    let s = s.strip_suffix('\n').unwrap_or(s);
    let s = s.strip_suffix('\r').unwrap_or(s);
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut chars = s.chars().peekable();
    let mut quoted = false;
    let mut closed = false;
    while let Some(c) = chars.next() {
        if quoted {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    quoted = false;
                    closed = true;
                }
            } else {
                field.push(c);
            }
        } else if c == ',' {
            fields.push(std::mem::take(&mut field));
            closed = false;
        } else if c == '"' && field.is_empty() && !closed {
            quoted = true;
        } else if closed || c == '"' {
            return Err(Error::new("invalid_artifact", "Malformed CSV quoting"));
        } else {
            field.push(c);
        }
    }
    if quoted {
        return Ok(None);
    }
    fields.push(field);
    Ok(Some(fields))
}
impl Op for ReadArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        Outcome::result(self.execute(ctx))
    }
}
