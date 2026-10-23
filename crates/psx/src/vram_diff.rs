//! `psx gpu-replay` and `psx vram-diff`: replay a recorded GP0 stream into
//! `psx-gpu`, and compare two VRAM dumps pixel by pixel.
//!
//! A stream file is little-endian u32 records `[n, word_1 .. word_n]`, one per
//! GP0 command, ending at n = 0 or end of file (the format written by
//! `tools/probes/gpu/gen_stream.py`).

use std::collections::BTreeMap;
use std::path::Path;

use psx_media::{MediaError, Result};
use serde_json::{Value, json};

const W: usize = 1024;
const H: usize = 512;

fn read_vram(path: &Path) -> Result<Vec<u16>> {
    let bytes = std::fs::read(path).map_err(|e| MediaError::io(path, e))?;
    if bytes.len() < W * H * 2 {
        return Err(MediaError::Malformed(format!(
            "{}: {} bytes, expected {}",
            path.display(),
            bytes.len(),
            W * H * 2
        )));
    }
    Ok(bytes[..W * H * 2].as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect())
}

/// Parse a stream file into commands.
pub fn read_stream(path: &Path) -> Result<Vec<Vec<u32>>> {
    let bytes = std::fs::read(path).map_err(|e| MediaError::io(path, e))?;
    let words: Vec<u32> = bytes.as_chunks::<4>().0.iter().map(|c| u32::from_le_bytes(*c)).collect();
    let mut cmds = Vec::new();
    let mut i = 0;
    while i < words.len() {
        let n = words[i] as usize;
        if n == 0 {
            break;
        }
        let end = i + 1 + n;
        if end > words.len() {
            return Err(MediaError::Malformed(format!("{}: record at word {i} runs past the end", path.display())));
        }
        cmds.push(words[i + 1..end].to_vec());
        i = end;
    }
    Ok(cmds)
}

pub fn replay(stream: &Path, out: &Path, png: Option<&Path>) -> Result<Value> {
    let cmds = read_stream(stream)?;
    let mut gpu = psx_gpu::Gpu::new();
    for c in &cmds {
        gpu.gp0_all(c);
    }
    std::fs::write(out, gpu.vram.to_bytes()).map_err(|e| MediaError::io(out, e))?;
    if let Some(p) = png {
        crate::run::write_png(p, 1024, 512, &gpu.vram.rgba15(0, 0, 1024, 512), png::ColorType::Rgba)
            .map_err(|e| MediaError::io(p, e))?;
    }
    Ok(json!({"commands": cmds.len(), "vram": out, "png": png}))
}

pub struct DiffArgs<'a> {
    pub a: &'a Path,
    pub b: &'a Path,
    /// Report counts per `cell`×`cell` block.
    pub cell: usize,
    /// Inclusive-exclusive region `(x, y, w, h)` to compare; whole VRAM otherwise.
    pub region: Option<(usize, usize, usize, usize)>,
    /// Write an image: matching pixels dimmed, differing ones bright red.
    pub png: Option<&'a Path>,
    /// Number of individual differing pixels listed.
    pub samples: usize,
}

pub fn diff(args: DiffArgs) -> Result<Value> {
    let (a, b) = (read_vram(args.a)?, read_vram(args.b)?);
    let (rx, ry, rw, rh) = args.region.unwrap_or((0, 0, W, H));
    let cell = args.cell.max(1);
    let mut total = 0usize;
    let mut cells: BTreeMap<(usize, usize), usize> = BTreeMap::new();
    // Per channel (r, g, b) signed delta b - a histogram, plus mask-bit-only differences.
    let mut deltas: [BTreeMap<i32, usize>; 3] = Default::default();
    let mut mask_only = 0usize;
    let mut bbox = (usize::MAX, usize::MAX, 0usize, 0usize);
    let mut samples = Vec::new();
    let mut img = args.png.map(|_| vec![0u8; W * H * 4]);
    for y in ry..(ry + rh).min(H) {
        for x in rx..(rx + rw).min(W) {
            let (pa, pb) = (a[y * W + x], b[y * W + x]);
            if let Some(img) = img.as_mut() {
                let [r, g, bl] = psx_gpu::vram::rgb15_to_rgb8(pa);
                let o = (y * W + x) * 4;
                img[o..o + 4].copy_from_slice(&if pa == pb { [r / 3, g / 3, bl / 3, 255] } else { [255, 32, 32, 255] });
            }
            if pa == pb {
                continue;
            }
            total += 1;
            *cells.entry((x / cell, y / cell)).or_default() += 1;
            bbox = (bbox.0.min(x), bbox.1.min(y), bbox.2.max(x), bbox.3.max(y));
            if pa & 0x7FFF == pb & 0x7FFF {
                mask_only += 1;
            }
            for (k, d) in deltas.iter_mut().enumerate() {
                let ch = |p: u16| ((p >> (5 * k)) & 31) as i32;
                let dv = ch(pb) - ch(pa);
                if dv != 0 {
                    *d.entry(dv).or_default() += 1;
                }
            }
            if samples.len() < args.samples {
                samples.push(json!({"x": x, "y": y, "a": format!("{pa:04x}"), "b": format!("{pb:04x}")}));
            }
        }
    }
    if let (Some(p), Some(img)) = (args.png, img) {
        crate::run::write_png(p, W as u32, H as u32, &img, png::ColorType::Rgba).map_err(|e| MediaError::io(p, e))?;
    }
    let hist = |m: &BTreeMap<i32, usize>| {
        m.iter().map(|(k, v)| (format!("{k:+}"), json!(v))).collect::<serde_json::Map<_, _>>()
    };
    Ok(json!({
        "a": args.a,
        "b": args.b,
        "region": {"x": rx, "y": ry, "w": rw, "h": rh},
        "pixels": rw.min(W - rx) * rh.min(H - ry),
        "differing": total,
        "mask_bit_only": mask_only,
        "bbox": if total > 0 { json!({"x0": bbox.0, "y0": bbox.1, "x1": bbox.2, "y1": bbox.3}) } else { Value::Null },
        "channel_delta_b_minus_a": {"r": hist(&deltas[0]), "g": hist(&deltas[1]), "b": hist(&deltas[2])},
        "cells": {
            "size": cell,
            "differing": cells.iter().map(|((cx, cy), n)| json!({"cx": cx, "cy": cy, "x": cx * cell, "y": cy * cell, "count": n})).collect::<Vec<_>>(),
        },
        "samples": samples,
        "png": args.png,
    }))
}
