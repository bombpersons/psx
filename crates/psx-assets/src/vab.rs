//! Sound banks and samples: VAB (`.VAB`, or a `.VH` header with a separate
//! `.VB` body) and VAG (PsyQ "File Formats"; psx-spx "SPU").
//!
//! VAG: a 48-byte big-endian header, then SPU-ADPCM blocks:
//! ```text
//! "VAGp", u32 version, u32 reserved, u32 data size, u32 sample rate, 12 reserved, char name[16]
//! ```
//!
//! VH (VAB header), little-endian:
//! ```text
//! 32 bytes  "pBAV", u32 version, u32 id, u32 file size (VH+VB), u16 reserved,
//!           u16 programs, u16 tones, u16 waveforms (VAGs), u8 volume, pan, attr1, attr2, u32 reserved
//! 128 x 16  program attributes: u8 tones, volume, priority, mode, pan, reserved; i16 attr; u32 x2 reserved
//! programs x 16 x 32  tone attributes (16 slots per program, see [`Tone`])
//! 256 x u16 waveform sizes in units of 8 bytes; entry 0 unused
//! ```
//! VB: the waveforms' ADPCM data back to back, in the order of the size table.

use serde::Serialize;

use crate::adpcm;

pub const VAG_MAGIC: &[u8; 4] = b"VAGp";
pub const VAB_MAGIC: &[u8; 4] = b"pBAV";
pub const VAG_HEADER: usize = 48;
/// How far past a VH to look for its VB (16-byte steps).
pub const VB_SEARCH: usize = 0x10000;

fn be32(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

fn le16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}

fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

#[derive(Clone, Debug, Serialize)]
pub struct Vag {
    pub version: u32,
    pub rate: u32,
    pub name: String,
    /// ADPCM bytes after the header (as declared, clipped to the input).
    pub data_size: usize,
    /// Header + data.
    pub size: usize,
}

impl Vag {
    pub fn parse(b: &[u8]) -> Result<Vag, String> {
        if b.len() < VAG_HEADER || &b[..4] != VAG_MAGIC {
            return Err("not a VAG (no VAGp header)".into());
        }
        let data_size = be32(b, 12) as usize;
        let rate = be32(b, 16);
        if !(1000..=192_000).contains(&rate) {
            return Err(format!("implausible VAG rate {rate}"));
        }
        if data_size == 0 || !data_size.is_multiple_of(16) || data_size > 16 << 20 {
            return Err(format!("implausible VAG data size {data_size}"));
        }
        let name = b[32..48].iter().take_while(|&&c| c != 0).map(|&c| if c.is_ascii_graphic() || c == b' ' { c as char } else { '?' }).collect();
        let avail = b.len() - VAG_HEADER;
        let data_size = data_size.min(avail);
        Ok(Vag { version: be32(b, 4), rate, name, data_size, size: VAG_HEADER + data_size })
    }

    pub fn decode(&self, b: &[u8]) -> adpcm::Decoded {
        adpcm::decode_stream(&b[VAG_HEADER..VAG_HEADER + self.data_size])
    }
}

/// One tone (key-split region) of a program.
#[derive(Clone, Debug, Serialize)]
pub struct Tone {
    pub priority: u8,
    /// 0 normal, 4 reverb applied.
    pub mode: u8,
    pub volume: u8,
    pub pan: u8,
    /// MIDI note at which the waveform plays at its recorded pitch (44.1 kHz SPU rate).
    pub center: u8,
    /// Fine tune (cents/100 x 128, roughly).
    pub shift: u8,
    pub min_note: u8,
    pub max_note: u8,
    pub pitch_bend: [u8; 2],
    pub adsr1: u16,
    pub adsr2: u16,
    pub program: i16,
    /// 1-based waveform (VAG) number.
    pub vag: i16,
}

#[derive(Clone, Debug, Serialize)]
pub struct Program {
    pub index: usize,
    pub volume: u8,
    pub pan: u8,
    pub tones: Vec<Tone>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Vab {
    pub version: u32,
    pub id: u32,
    /// Declared VH+VB size.
    pub file_size: u32,
    pub volume: u8,
    pub pan: u8,
    pub programs: Vec<Program>,
    /// Byte sizes of waveforms 1..=n in the VB.
    pub vag_sizes: Vec<usize>,
    /// Size of the VH part.
    pub header_size: usize,
    /// Sum of waveform sizes (the VB size).
    pub body_size: usize,
}

impl Vab {
    /// Parse a VH (the start of a `.VAB` or a `.VH`). The VB may follow
    /// (`header_size + body_size == file_size`) or live in another file.
    pub fn parse(b: &[u8]) -> Result<Vab, String> {
        if b.len() < 32 || &b[..4] != VAB_MAGIC {
            return Err("not a VAB header (no pBAV)".into());
        }
        let (programs, tones, vags) = (le16(b, 18) as usize, le16(b, 20) as usize, le16(b, 22) as usize);
        if programs == 0 || programs > 128 || vags > 254 || tones > programs * 16 {
            return Err(format!("implausible VAB counts: {programs} programs, {tones} tones, {vags} waveforms"));
        }
        let header_size = 32 + 128 * 16 + programs * 16 * 32 + 512;
        if b.len() < header_size {
            return Err(format!("VH truncated ({} of {header_size} bytes)", b.len()));
        }
        let tone_base = 32 + 128 * 16;
        let size_base = tone_base + programs * 16 * 32;
        let vag_sizes: Vec<usize> = (1..=vags).map(|i| le16(b, size_base + 2 * i) as usize * 8).collect();
        let body_size: usize = vag_sizes.iter().sum();
        let file_size = le32(b, 12);
        // A VH's size field covers the VB too. Some tools pad the VB, so
        // require it to cover at least the waveform sum.
        if (file_size as usize) < header_size + body_size {
            return Err(format!("VAB size {file_size} < header {header_size} + waveforms {body_size}"));
        }
        // Programs with tones occupy consecutive tone-attribute slots of 16.
        let mut progs = Vec::new();
        let mut slot = 0;
        for p in 0..128 {
            let pa = 32 + p * 16;
            let n = b[pa] as usize;
            if n == 0 {
                continue;
            }
            if n > 16 || slot >= programs {
                return Err(format!("program {p}: {n} tones beyond the tone table"));
            }
            let mut t = Vec::new();
            for k in 0..n {
                let o = tone_base + (slot * 16 + k) * 32;
                let tone = Tone {
                    priority: b[o],
                    mode: b[o + 1],
                    volume: b[o + 2],
                    pan: b[o + 3],
                    center: b[o + 4],
                    shift: b[o + 5],
                    min_note: b[o + 6],
                    max_note: b[o + 7],
                    pitch_bend: [b[o + 12], b[o + 13]],
                    adsr1: le16(b, o + 16),
                    adsr2: le16(b, o + 18),
                    program: le16(b, o + 20) as i16,
                    vag: le16(b, o + 22) as i16,
                };
                if tone.vag < 0 || tone.vag as usize > vags || tone.min_note > 127 || tone.max_note > 127 || tone.center > 127 {
                    return Err(format!("program {p} tone {k}: implausible fields (vag {})", tone.vag));
                }
                t.push(tone);
            }
            slot += 1;
            progs.push(Program { index: p, volume: b[pa + 1], pan: b[pa + 4], tones: t });
        }
        Ok(Vab {
            version: le32(b, 4),
            id: le32(b, 8),
            file_size,
            volume: b[24],
            pan: b[25],
            programs: progs,
            vag_sizes,
            header_size,
            body_size,
        })
    }

    /// Where the VB starts in `b` (the bytes starting at the VH), if it is
    /// there in full: right after the VH, after a u32 holding the VB size
    /// (Croc's archives store `VH, u32 vb_size, VB`), or after padding to a
    /// 16-byte boundary within [`VB_SEARCH`] bytes (Mega Man 8's `.PAC`s).
    /// Every waveform must look like SPU-ADPCM ending in an END block, so a VH
    /// whose waveforms live elsewhere is not paired with whatever follows it.
    pub fn body_offset(&self, b: &[u8]) -> Option<usize> {
        let h = self.header_size;
        if self.body_size == 0 {
            return None;
        }
        let ranges = self.vag_ranges();
        let fits = |at: usize| {
            let Some(body) = b.get(at..at + self.body_size) else { return false };
            ranges.iter().all(|r| adpcm::plausible_sample(&body[r.clone()]))
        };
        let prefixed = b.get(h..h + 4).is_some_and(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]]) as usize == self.body_size);
        if prefixed && fits(h + 4) {
            return Some(h + 4);
        }
        (h..h + VB_SEARCH).step_by(16).find(|&at| fits(at))
    }

    /// Check a separately stored VB (e.g. a `.VB` next to a `.VH`) the same way.
    pub fn body_matches(&self, vb: &[u8]) -> bool {
        vb.len() >= self.body_size && self.vag_ranges().into_iter().all(|r| adpcm::plausible_sample(&vb[r]))
    }

    /// Byte ranges of each waveform within a VB.
    pub fn vag_ranges(&self) -> Vec<std::ops::Range<usize>> {
        let mut at = 0;
        self.vag_sizes
            .iter()
            .map(|&s| {
                let r = at..at + s;
                at += s;
                r
            })
            .collect()
    }
}

/// Mono 16-bit PCM WAV bytes.
pub fn wav_mono(pcm: &[i16], rate: u32) -> Vec<u8> {
    let data_len = (pcm.len() * 2) as u32;
    let mut b = Vec::with_capacity(44 + data_len as usize);
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data_len).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&rate.to_le_bytes());
    b.extend_from_slice(&(rate * 2).to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data_len.to_le_bytes());
    for s in pcm {
        b.extend_from_slice(&s.to_le_bytes());
    }
    b
}

/// Peak and RMS of PCM (full scale = 1.0), for sanity checks.
pub fn levels(pcm: &[i16]) -> (f64, f64) {
    if pcm.is_empty() {
        return (0.0, 0.0);
    }
    let peak = pcm.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0) as f64 / 32768.0;
    let rms = (pcm.iter().map(|&s| (s as f64).powi(2)).sum::<f64>() / pcm.len() as f64).sqrt() / 32768.0;
    (peak, rms)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::adpcm::{block, FLAG_END};

    pub fn vag_bytes(rate: u32) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(VAG_MAGIC);
        b.extend_from_slice(&0x20u32.to_be_bytes());
        b.extend_from_slice(&0u32.to_be_bytes());
        b.extend_from_slice(&48u32.to_be_bytes());
        b.extend_from_slice(&rate.to_be_bytes());
        b.extend_from_slice(&[0; 12]);
        let mut name = [0u8; 16];
        name[..4].copy_from_slice(b"BEEP");
        b.extend_from_slice(&name);
        let mut nib = [0u8; 28];
        for (i, n) in nib.iter_mut().enumerate() {
            *n = if i % 4 < 2 { 4 } else { 0xC };
        }
        b.extend_from_slice(&[0; 16]);
        b.extend_from_slice(&block(0x02, 0, &nib));
        b.extend_from_slice(&block(0x02, FLAG_END, &nib));
        b
    }

    /// A VAB with one program of two tones over two 32-byte waveforms.
    pub fn vab_bytes() -> Vec<u8> {
        let programs = 1;
        let header = 32 + 128 * 16 + programs * 16 * 32 + 512;
        let mut b = vec![0u8; header];
        b[..4].copy_from_slice(VAB_MAGIC);
        b[4..8].copy_from_slice(&7u32.to_le_bytes());
        b[12..16].copy_from_slice(&((header + 64) as u32).to_le_bytes());
        b[18..20].copy_from_slice(&1u16.to_le_bytes());
        b[20..22].copy_from_slice(&2u16.to_le_bytes());
        b[22..24].copy_from_slice(&2u16.to_le_bytes());
        b[24] = 127;
        b[32] = 2; // program 0: 2 tones
        b[33] = 100;
        let tb = 32 + 128 * 16;
        for (k, (center, lo, hi, vag)) in [(60u8, 0u8, 63u8, 1i16), (72, 64, 127, 2)].into_iter().enumerate() {
            let o = tb + k * 32;
            b[o + 2] = 127;
            b[o + 4] = center;
            b[o + 6] = lo;
            b[o + 7] = hi;
            b[o + 22..o + 24].copy_from_slice(&vag.to_le_bytes());
        }
        let sb = tb + programs * 16 * 32;
        b[sb + 2..sb + 4].copy_from_slice(&4u16.to_le_bytes());
        b[sb + 4..sb + 6].copy_from_slice(&4u16.to_le_bytes());
        let nib = [3u8; 28];
        for _ in 0..2 {
            b.extend_from_slice(&block(0x08, 0, &nib));
            b.extend_from_slice(&block(0x08, FLAG_END, &nib));
        }
        b
    }

    #[test]
    fn vag_header_and_decode() {
        let b = vag_bytes(22050);
        let v = Vag::parse(&b).unwrap();
        assert_eq!((v.rate, v.name.as_str(), v.size), (22050, "BEEP", b.len()));
        let d = v.decode(&b);
        assert_eq!(d.pcm.len(), 84);
        assert!(d.pcm[28..].iter().any(|&s| s != 0));
        assert!(Vag::parse(&b[..40]).is_err());
    }

    #[test]
    fn vab_programs_tones_and_waveforms() {
        let b = vab_bytes();
        let v = Vab::parse(&b).unwrap();
        assert_eq!(v.programs.len(), 1);
        let p = &v.programs[0];
        assert_eq!((p.volume, p.tones.len()), (100, 2));
        assert_eq!((p.tones[1].center, p.tones[1].min_note, p.tones[1].vag), (72, 64, 2));
        assert_eq!(v.vag_sizes, vec![32, 32]);
        assert_eq!(v.header_size + v.body_size, b.len());
        let r = v.vag_ranges();
        assert_eq!(r[1], 32..64);
        let pcm = adpcm::decode_stream(&b[v.header_size..][r[0].clone()]).pcm;
        assert_eq!(pcm[0], (3 << 12) >> 8);
        assert_eq!(v.body_offset(&b), Some(v.header_size));
        // Croc-style: VH, u32 VB size, VB.
        let mut prefixed = b[..v.header_size].to_vec();
        prefixed.extend_from_slice(&(v.body_size as u32).to_le_bytes());
        prefixed.extend_from_slice(&b[v.header_size..]);
        assert_eq!(v.body_offset(&prefixed), Some(v.header_size + 4));
        assert_eq!(v.body_offset(&b[..b.len() - 1]), None);
        let mut bad = b.clone();
        bad[12..16].copy_from_slice(&100u32.to_le_bytes());
        assert!(Vab::parse(&bad).is_err());
    }

    #[test]
    fn wav_header_is_44_bytes() {
        let w = wav_mono(&[1, -1], 44100);
        assert_eq!(w.len(), 48);
        assert_eq!(&w[..4], b"RIFF");
        let (peak, rms) = levels(&[16384, -16384]);
        assert!((peak - 0.5).abs() < 1e-9 && (rms - 0.5).abs() < 1e-9);
    }
}
