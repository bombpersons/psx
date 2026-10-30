//! PS1 CPU address classification (psx-spx "Memory Map").
//!
//! A virtual address keeps its segment because KSEG0 (cached) and KSEG1
//! (uncached) reach the same storage with different behaviour. Callers should
//! report both the virtual address and the resolved region/offset.

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Segment {
    Kuseg,
    Kseg0,
    Kseg1,
    Kseg2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Region {
    /// 2 MiB main RAM, mirrored four times across the first 8 MiB.
    MainRam,
    Expansion1,
    /// 1 KiB data cache used as fast RAM.
    Scratchpad,
    Io,
    Expansion2,
    Expansion3,
    Bios,
    /// KSEG2 cache control (0xFFFE0000).
    CacheControl,
    Unmapped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Resolved {
    pub virt: u32,
    pub segment: Segment,
    pub physical: u32,
    pub region: Region,
    /// Offset inside the region's backing storage (mirrors folded).
    pub offset: u32,
}

/// Parse a hex address or value, with or without `0x`/`0X`; surrounding spaces are ignored.
pub fn parse_hex(s: &str) -> Option<u32> {
    let s = s.trim();
    u32::from_str_radix(s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")).unwrap_or(s), 16).ok()
}

/// An address as psx prints it: `0x` and eight upper-case hex digits.
pub fn hex(a: u32) -> String {
    format!("0x{a:08X}")
}

/// Serde for an address field: written as [`hex`], read from that, from hex
/// without `0x`, or from a number (files psx wrote before addresses were hex).
pub mod addr {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(a: &u32, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&super::hex(*a))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
        Ok(Repr::deserialize(d)?.0)
    }

    struct Repr(u32);

    impl<'de> Deserialize<'de> for Repr {
        fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            #[derive(Deserialize)]
            #[serde(untagged)]
            enum Either {
                Number(u32),
                Text(String),
            }
            match Either::deserialize(d)? {
                Either::Number(n) => Ok(Repr(n)),
                Either::Text(t) => super::parse_hex(&t)
                    .map(Repr)
                    .ok_or_else(|| serde::de::Error::custom(format!("{t:?} is not a hex address"))),
            }
        }
    }

    /// The same for `Option<u32>`.
    pub mod option {
        use serde::{Deserialize, Deserializer, Serializer};

        pub fn serialize<S: Serializer>(a: &Option<u32>, s: S) -> Result<S::Ok, S::Error> {
            match a {
                Some(a) => super::serialize(a, s),
                None => s.serialize_none(),
            }
        }

        pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u32>, D::Error> {
            Ok(Option::<super::Repr>::deserialize(d)?.map(|r| r.0))
        }
    }

    /// The same for a list (or, serializing, an array) of addresses.
    pub mod vec {
        use serde::{Deserialize, Deserializer, Serializer};

        pub fn serialize<S: Serializer, A: AsRef<[u32]>>(a: &A, s: S) -> Result<S::Ok, S::Error> {
            s.collect_seq(a.as_ref().iter().map(|a| super::super::hex(*a)))
        }

        pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u32>, D::Error> {
            Ok(Vec::<super::Repr>::deserialize(d)?.into_iter().map(|r| r.0).collect())
        }
    }
}

pub const MAIN_RAM_SIZE: u32 = 2 * 1024 * 1024;

/// The region an address reaches, as psx prints it beside data addresses.
pub fn region(virt: u32) -> Region {
    resolve(virt).region
}

pub fn resolve(virt: u32) -> Resolved {
    let segment = match virt >> 29 {
        0..=3 => Segment::Kuseg,
        4 => Segment::Kseg0,
        5 => Segment::Kseg1,
        _ => Segment::Kseg2,
    };
    if segment == Segment::Kseg2 {
        let region = if virt == 0xFFFE_0130 { Region::CacheControl } else { Region::Unmapped };
        return Resolved { virt, segment, physical: virt, region, offset: virt.wrapping_sub(0xFFFE_0000) };
    }
    let physical = virt & 0x1FFF_FFFF;
    let (region, offset) = match physical {
        0x0000_0000..=0x007F_FFFF => (Region::MainRam, physical % MAIN_RAM_SIZE),
        0x1F00_0000..=0x1F7F_FFFF => (Region::Expansion1, physical - 0x1F00_0000),
        // Scratchpad is not reachable through KSEG1.
        0x1F80_0000..=0x1F80_03FF if segment != Segment::Kseg1 => (Region::Scratchpad, physical - 0x1F80_0000),
        0x1F80_1000..=0x1F80_1FFF => (Region::Io, physical - 0x1F80_1000),
        0x1F80_2000..=0x1F80_3FFF => (Region::Expansion2, physical - 0x1F80_2000),
        0x1FA0_0000..=0x1FBF_FFFF => (Region::Expansion3, physical - 0x1FA0_0000),
        0x1FC0_0000..=0x1FC7_FFFF => (Region::Bios, physical - 0x1FC0_0000),
        _ => (Region::Unmapped, physical),
    };
    Resolved { virt, segment, physical, region, offset }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_ram_mirrors_but_keeps_segment() {
        let a = resolve(0x8001_0000);
        let b = resolve(0xA061_0000);
        assert_eq!((a.region, a.offset), (Region::MainRam, 0x1_0000));
        assert_eq!((b.region, b.offset), (Region::MainRam, 0x1_0000));
        assert_ne!(a.segment, b.segment);
        assert_eq!(resolve(0x1F80_0010).region, Region::Scratchpad);
        assert_eq!(resolve(0xBF80_0010).region, Region::Unmapped);
        assert_eq!(resolve(0xBFC0_0000).region, Region::Bios);
    }
}
