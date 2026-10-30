//! `psx assets diff`: compare two VRAM dumps pixel by pixel.

use std::collections::BTreeMap;
use std::path::Path;

use crate::commands::{Ctx, Op, Outcome};
use crate::{Error, Result};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::PathBuf;

fn d_cell() -> usize {
    128
}
fn d_samples() -> usize {
    16
}

#[derive(clap::Args, Deserialize, JsonSchema)]
pub struct VramDiffArgs {
    /// First raw VRAM dump (vram.bin).
    pub a: PathBuf,
    /// Second raw VRAM dump.
    pub b: PathBuf,
    /// Cell size for per-block counts.
    #[arg(long, default_value_t = d_cell())]
    #[serde(default = "d_cell")]
    pub cell: usize,
    /// Region `X,Y,W,H` to compare (default: all of VRAM).
    #[arg(long)]
    pub region: Option<String>,
    /// Write a diff image (differing pixels red).
    #[arg(long)]
    pub png: Option<PathBuf>,
    /// Differing pixels listed individually.
    #[arg(long, default_value_t = d_samples())]
    #[serde(default = "d_samples")]
    pub samples: usize,
}

impl Op for VramDiffArgs {
    fn exec(self, _: &Ctx) -> Outcome {
        let VramDiffArgs { a, b, cell, region, png, samples } = self;
        Outcome::result((|| -> crate::Result<Value> {
            let region = match region {
                Some(r) => {
                    let v: Vec<usize> = r.split(',').filter_map(|s| s.trim().parse().ok()).collect();
                    if v.len() != 4 {
                        return Err(Error::invalid(format!("bad region {r}, expected X,Y,W,H")));
                    }
                    Some((v[0], v[1], v[2], v[3]))
                }
                None => None,
            };
            diff(DiffArgs { a: &a, b: &b, cell, region, png: png.as_deref(), samples })
        })())
    }
}

const W: usize = 1024;
const H: usize = 512;

fn read_vram(path: &Path) -> Result<Vec<u16>> {
    let bytes = std::fs::read(path).map_err(|e| Error::io(path, e))?;
    if bytes.len() < W * H * 2 {
        return Err(Error::malformed(format!("{}: {} bytes, expected {}", path.display(), bytes.len(), W * H * 2)));
    }
    Ok(bytes[..W * H * 2].as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect())
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
        crate::run::write_png(p, W as u32, H as u32, &img, png::ColorType::Rgba).map_err(|e| Error::io(p, e))?;
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
