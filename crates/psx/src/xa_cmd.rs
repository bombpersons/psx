//! `psx xa`: list a file's XA-ADPCM channels, or decode one to a WAV.

use crate::Error;
use std::collections::{BTreeMap, VecDeque};
use std::path::Path;

use crate::commands::{Ctx, Op, Outcome};
use psx_core::xa::{Coding, XaDecoder};
use psx_media::Disc;
use psx_media::iso9660::FileSystem;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::PathBuf;

#[derive(clap::Args, Deserialize, JsonSchema)]
pub struct XaArgs {
    /// Disc image: .cue, raw .bin, .iso or .chd.
    pub image: PathBuf,
    /// ISO path of the stream, e.g. `/MUSIC.XA`.
    pub path: String,
    /// Channel to decode (needs -o).
    #[arg(long)]
    pub channel: Option<u8>,
    /// Also require this subheader file number.
    #[arg(long)]
    pub file: Option<u8>,
    /// WAV output path for the decoded channel.
    #[arg(short, long)]
    pub out: Option<PathBuf>,
}

impl Op for XaArgs {
    fn exec(self, _: &Ctx) -> Outcome {
        Outcome::result(run(&self.image, &self.path, self.file, self.channel, self.out.as_deref()))
    }
}

pub fn run(
    image: &Path,
    path: &str,
    file: Option<u8>,
    channel: Option<u8>,
    out: Option<&Path>,
) -> crate::Result<Value> {
    let disc = Disc::open(image)?;
    let fs = FileSystem::read(&disc)?;
    let entry = fs.entry(path)?;
    // (file, channel) -> (coding byte, sectors, first lba)
    let mut channels: BTreeMap<(u8, u8), (u8, u32, u32)> = BTreeMap::new();
    let mut sectors = Vec::new();
    for lba in entry.lba..entry.lba + entry.sectors() {
        let (raw, _) = disc.read_raw(lba)?;
        if raw.len() != 2352 || raw[15] != 2 || raw[18] & 0x04 == 0 {
            continue;
        }
        let key = (raw[16], raw[17]);
        let c = channels.entry(key).or_insert((raw[19], 0, lba));
        c.1 += 1;
        if Some(key.1) == channel && file.is_none_or(|f| f == key.0) {
            sectors.push(raw);
        }
    }
    let listed: Vec<Value> = channels
        .iter()
        .map(|(&(f, ch), &(coding, n, first))| {
            let c = Coding::from_byte(coding);
            let per_sector = if c.bits == 4 { 4032 } else { 2016 } / if c.stereo { 2 } else { 1 };
            json!({
                "file": f, "channel": ch, "coding": format!("0x{coding:02X}"),
                "stereo": c.stereo, "rate": c.rate, "bits": c.bits,
                "sectors": n, "first_lba": first,
                "seconds": (n as f64 * per_sector as f64 / c.rate as f64 * 100.0).round() / 100.0,
            })
        })
        .collect();
    let mut result = json!({"path": entry.path, "lba": entry.lba, "sectors": entry.sectors(), "channels": listed});
    if let Some(ch) = channel {
        let Some(out) = out else { return Err(Error::invalid("--channel needs -o OUT.wav")) };
        if sectors.is_empty() {
            return Err(Error::new("not_found", format!("no XA audio sectors on channel {ch}")));
        }
        let mut dec = XaDecoder::default();
        let mut pcm = VecDeque::new();
        for raw in &sectors {
            let coding = Coding::from_byte(raw[19]);
            let native = dec.decode(raw, coding);
            dec.resample(&native, coding.rate, &mut pcm);
        }
        let samples: Vec<i16> = pcm.iter().flat_map(|&(l, r)| [l, r]).collect();
        crate::native::write_wav(out, &samples).map_err(|e| Error::io(out, e))?;
        let peak = samples.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
        result["wav"] =
            json!({"path": out, "sectors": sectors.len(), "seconds": pcm.len() as f64 / 44_100.0, "peak": peak});
    }
    Ok(result)
}
