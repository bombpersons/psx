//! `SYSTEM.CNF`: tells the BIOS which executable to boot and how to set up the
//! kernel. When absent, the BIOS boots `cdrom:PSX.EXE;1`.

use std::collections::BTreeMap;

#[derive(Clone, Debug, serde::Serialize)]
pub struct SystemCnf {
    /// Raw `KEY = VALUE` pairs, keys upper-cased.
    pub fields: BTreeMap<String, String>,
    pub boot: Option<String>,
    /// Arguments after the BOOT path (rarely used).
    pub boot_args: Vec<String>,
    pub tcb: Option<u32>,
    pub event: Option<u32>,
    pub stack: Option<u32>,
}

pub const DEFAULT_BOOT: &str = "cdrom:PSX.EXE;1";

impl SystemCnf {
    pub fn parse(bytes: &[u8]) -> Self {
        let text = String::from_utf8_lossy(bytes);
        let mut fields = BTreeMap::new();
        for line in text.lines() {
            if let Some((k, v)) = line.split_once('=') {
                fields.insert(k.trim().to_ascii_uppercase(), v.trim().to_string());
            }
        }
        let mut boot_words = fields.get("BOOT").map(|b| b.split_whitespace()).into_iter().flatten();
        let boot = boot_words.next().map(str::to_string);
        let boot_args = boot_words.map(str::to_string).collect();
        SystemCnf {
            tcb: fields.get("TCB").and_then(|v| crate::memmap::parse_hex(v)),
            event: fields.get("EVENT").and_then(|v| crate::memmap::parse_hex(v)),
            stack: fields.get("STACK").and_then(|v| crate::memmap::parse_hex(v)),
            boot,
            boot_args,
            fields,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typical_file() {
        let c = SystemCnf::parse(b"BOOT = cdrom:\\SLUS_002.38;1\r\nTCB = 4\r\nEVENT = 10\r\nSTACK = 801FFFF0\r\n");
        assert_eq!(c.boot.as_deref(), Some("cdrom:\\SLUS_002.38;1"));
        assert_eq!(c.tcb, Some(4));
        assert_eq!(c.event, Some(0x10));
        assert_eq!(c.stack, Some(0x801F_FFF0));
    }
}
