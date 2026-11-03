//! `run --pad-reads`: where the game keeps its pad, and the code that reads
//! it (`psx_core::padflow`), for the agent to look at a game's input.

use std::collections::{BTreeMap, BTreeSet};

use psx_core::Machine;
use psx_core::cpu::StepInfo;
use psx_core::padflow::{BufferByte, PadFlow};
use serde_json::{Value, json};

use crate::symbols_io::ScopedSymbols;

/// What a reply byte holds, by its position in a poll.
pub(crate) fn byte_name(pos: u8) -> &'static str {
    match pos {
        1 => "id",
        2 => "ready",
        3 => "buttons_lo",
        4 => "buttons_hi",
        5 => "rx",
        6 => "ry",
        7 => "lx",
        8 => "ly",
        _ => "other",
    }
}

#[derive(Default)]
struct Reader {
    count: u64,
    bytes: BTreeSet<u8>,
    addresses: BTreeSet<u32>,
}

pub(crate) struct PadReads {
    flow: PadFlow,
    polls: u64,
    fields: u64,
    without_read: u64,
    read_this_field: bool,
    frame: Option<u64>,
    readers: BTreeMap<u32, Reader>,
}

impl PadReads {
    pub fn new() -> PadReads {
        PadReads {
            flow: PadFlow::new(),
            polls: 0,
            fields: 0,
            without_read: 0,
            read_this_field: false,
            frame: None,
            readers: BTreeMap::new(),
        }
    }

    /// The step `info` just ran on `m`.
    pub fn step(&mut self, info: &StepInfo, m: &mut Machine) {
        let sio = &mut m.bus.sio0;
        self.polls += sio.polls.get_or_insert_with(Vec::new).drain(..).count() as u64;
        self.flow.step(info, sio);
        let frame = m.bus.frames;
        if self.frame != Some(frame) {
            if self.frame.is_some() {
                self.fields += 1;
                self.without_read += !self.read_this_field as u64;
            }
            self.frame = Some(frame);
            self.read_this_field = false;
        }
        for r in self.flow.take_reads() {
            self.read_this_field = true;
            let e = self.readers.entry(r.pc).or_default();
            e.count += 1;
            e.bytes.extend(r.marks.iter().map(|k| k.pos));
            e.addresses.insert(r.addr);
        }
    }

    pub fn report(&self, scopes: &[ScopedSymbols], m: &Machine) -> Value {
        let bytes = |b: Vec<BufferByte>| {
            b.iter()
                .map(|b| json!({"address": format!("0x{:08X}", b.addr), "byte": byte_name(b.pos)}))
                .collect::<Vec<_>>()
        };
        let mut top: Vec<(&u32, &Reader)> = self.readers.iter().collect();
        top.sort_by_key(|(pc, r)| (std::cmp::Reverse(r.count), **pc));
        json!({
            "buffer": bytes(self.flow.buffer()),
            "copies": bytes(self.flow.copies()),
            "polls": self.polls,
            "fields": self.fields,
            "fields_without_read": self.without_read,
            "readers": top.iter().take(16).map(|(pc, r)| json!({
                "pc": format!("0x{pc:08X}"),
                "function": crate::final_name(scopes, **pc, Some(&m.bus.ram)),
                "count": r.count,
                "bytes": r.bytes.iter().map(|p| byte_name(*p)).collect::<Vec<_>>(),
                "addresses": r.addresses.iter().take(8).map(|a| format!("0x{a:08X}")).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use psx_core::padflow::test_program::{Keep, machine};

    #[test]
    fn the_report_names_the_buffer_and_its_readers() {
        let mut m = machine(Keep::Bytes, psx_core::sio0::button::CROSS);
        let mut p = PadReads::new();
        for _ in 0..200_000 {
            let info = m.step();
            p.step(&info, &mut m);
        }
        let r = p.report(&[], &m);
        assert_eq!(
            r["buffer"],
            json!([{"address": "0x80020000", "byte": "buttons_lo"}, {"address": "0x80020001", "byte": "buttons_hi"}])
        );
        assert!(r["polls"].as_u64().unwrap() > 2);
        let readers: Vec<&str> = r["readers"].as_array().unwrap().iter().map(|x| x["pc"].as_str().unwrap()).collect();
        assert_eq!(readers.len(), 2, "{readers:?}");
        assert!(readers.contains(&"0x80010008") && readers.contains(&"0x8001000C"), "{readers:?}");
    }
}
