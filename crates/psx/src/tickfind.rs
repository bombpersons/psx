//! Finding a game's tick (`run --find-tick`): the stores of one run grouped by
//! the fields they fell in. A game's logic stores its variables once a tick,
//! so the largest group whose stores per field vary (0, 1, 2 ...) is usually
//! the tick's, and a counter in it is a `[tick]` for the field map.

use std::collections::HashMap;

use psx_core::observe::{Census, Pair};
use serde_json::{Value, json};

/// Groups in the report, and candidates per group; the file holds more.
const SHOWN: (usize, usize) = (6, 4);
const FILED: (usize, usize) = (40, 5000);

/// How a pair's stores change the value, best tick candidate first: a
/// counter or steady step only this pair moves, then a steady step that
/// other code resets (a flag set once a pass, an accumulator).
fn kind(key: u64, p: &Pair) -> (u8, &'static str) {
    let steady = p.same_step == p.stores && p.first_step != 0;
    let moved = p.last.wrapping_sub(p.first_old) == p.first_step.wrapping_mul(p.stores as u32) & mask(key);
    if steady && moved && p.first_step == 1 {
        (0, "counter")
    } else if steady && moved {
        (1, "step")
    } else if steady {
        (2, "stepped")
    } else if p.changes == p.stores {
        (3, "changes")
    } else if p.changes > 0 {
        (4, "varies")
    } else {
        (5, "same")
    }
}

fn mask(key: u64) -> u32 {
    u32::MAX >> (32 - 8 * Census::parts(key).1.clamp(1, 4))
}

fn kseg(addr: u32) -> u32 {
    if addr < 0x0080_0000 { 0x8000_0000 | addr } else { 0xA000_0000 | addr }
}

fn candidate(key: u64, p: &Pair) -> Value {
    let (addr, width, pc) = Census::parts(key);
    let (_, kind) = kind(key, p);
    let mut c = json!({"write": format!("{:08X}:{width}", kseg(addr)), "pc": format!("{:08X}", kseg(pc)), "kind": kind, "stores": p.stores});
    if kind == "step" || kind == "stepped" {
        let shift = 32 - 8 * width;
        c["step"] = json!(((p.first_step << shift) as i32) >> shift);
    }
    c
}

fn distinct(pairs: &[(u64, &Pair)], part: impl Fn((u32, u32, u32)) -> u32) -> usize {
    let mut v: Vec<u32> = pairs.iter().map(|(k, _)| part(Census::parts(*k))).collect();
    v.sort_unstable();
    v.dedup();
    v.len()
}

fn group(fields: u32, pairs: &[(u64, &Pair)], shown: usize) -> Value {
    let by = pairs[0].1.by_count;
    let none = fields - by.iter().sum::<u32>();
    let mut hist = serde_json::Map::new();
    for (n, count) in [none, by[0], by[1], by[2]].into_iter().enumerate().filter(|(_, c)| *c > 0) {
        hist.insert(n.to_string(), json!(count));
    }
    json!({
        "pairs": pairs.len(), "pcs": distinct(pairs, |(_, _, pc)| pc), "addresses": distinct(pairs, |(a, _, _)| a),
        "fields_by_stores": hist,
        "every_field": by[0] == fields,
        "candidates": pairs.iter().take(shown).map(|(k, p)| candidate(*k, p)).collect::<Vec<_>>(),
    })
}

/// The report section, and the file's contents.
pub fn report(c: &Census) -> (Value, Value) {
    let fields = c.fields;
    let enough = (fields / 5).max(2);
    let mut groups: HashMap<u64, Vec<(u64, &Pair)>> = HashMap::new();
    for (&k, p) in &c.pairs {
        let stored = p.by_count.iter().sum::<u32>();
        // The kernel's own stores (its VSync counter among them) are no game's tick.
        let kernel = Census::parts(k).2 < 0x1_0000 || Census::parts(k).2 >= 0x1FC0_0000;
        if stored >= enough && p.by_count[3] == 0 && !kernel {
            groups.entry(p.signature).or_default().push((k, p));
        }
    }
    let mut groups: Vec<Vec<(u64, &Pair)>> = groups.into_values().collect();
    for g in &mut groups {
        g.sort_by_key(|(k, p)| (kind(*k, p).0, Census::parts(*k).0, *k));
    }
    groups.sort_by_cached_key(|g| (std::cmp::Reverse(distinct(g, |(_, _, pc)| pc)), g[0].0));
    let show = |n: (usize, usize)| groups.iter().take(n.0).map(|g| group(fields, g, n.1)).collect::<Vec<_>>();
    let how = "CPU stores to main RAM, grouped by the fields they fell in and how many each: \
        pairs (PC, address) in one group were stored in exactly the same fields, as many times. \
        Groups are ranked by how many storing PCs share the pattern (the code that runs to it, \
        where a buffer filled by one loop has many addresses but few PCs); a game's logic \
        stores its variables once a tick, so the tick's group is usually the first whose fields_by_stores \
        varies (a group stored once in every field is the field rate). In that group a counter \
        (or a steady step) is a [tick] for the field map: write = its write, pc = its pc. \
        Check it as guide topic fields says.";
    let mut warnings = Vec::new();
    if fields < 60 {
        warnings.push("fewer than 60 fields: run longer, in play".to_string());
    }
    let section =
        json!({"fields": fields, "pairs": c.pairs.len(), "groups": show(SHOWN), "how": how, "warnings": warnings});
    let file = json!({"fields": fields, "pairs": c.pairs.len(), "groups": show(FILED), "how": how});
    (section, file)
}

#[cfg(test)]
mod tests {
    use psx_core::observe::{Observer, WriteHit, Writer};

    fn put(o: &mut Observer, address: u32, pc: u32, old: u32, value: u32) {
        let (frame, cycle, width, dma_channel) = (0, 0, 4, None);
        let (pc, value, old) = (Some(pc), Some(value), Some(old));
        o.record(WriteHit { frame, cycle, writer: Writer::Cpu, address, width, pc, value, old, dma_channel });
    }

    #[test]
    fn the_tick_group_comes_first_with_its_counter() {
        let mut o = Observer { census: Some(Default::default()), ..Default::default() };
        let mut ticks = 0;
        for field in 0..60u32 {
            // A logic tick every other field (two in field 31, catching up),
            // storing a counter, a flag another PC clears, and three variables.
            for _ in 0..[field % 2, 2][usize::from(field == 31)] {
                put(&mut o, 0x8008_0000, 0x8001_0000, ticks, ticks + 1);
                put(&mut o, 0x8008_0004, 0x8001_0004, 0, 1);
                put(&mut o, 0x8008_0004, 0x8001_0008, 1, 0);
                for v in 0..3 {
                    put(&mut o, 0x8008_0010 + 4 * v, 0x8001_0010 + 4 * v, field, field + 7);
                }
                ticks += 1;
            }
            // The kernel's vsync counter, and a frame count of the game's own.
            put(&mut o, 0x8000_9000, 0x8000_4000, field, field + 1);
            put(&mut o, 0x8008_0100, 0x8001_0100, field, field + 1);
            o.census.as_mut().unwrap().end_field();
        }
        let (section, file) = super::report(o.census.as_ref().unwrap());
        let groups = section["groups"].as_array().unwrap();
        let tick = &groups[0];
        assert_eq!(tick["pcs"], 6);
        assert_eq!(tick["fields_by_stores"], serde_json::json!({"0": 30, "1": 29, "2": 1}));
        let c = &tick["candidates"];
        assert_eq!(
            (&c[0]["write"], &c[0]["pc"], &c[0]["kind"]),
            (&"80080000:4".into(), &"80010000".into(), &"counter".into())
        );
        assert_eq!((&c[1]["kind"], &c[1]["step"]), (&"stepped".into(), &1.into()));
        // Only the game's frame count remains beside it: the kernel is left out.
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[1]["every_field"], true);
        assert_eq!(file["groups"][0]["candidates"].as_array().unwrap().len(), 6);
    }
}
