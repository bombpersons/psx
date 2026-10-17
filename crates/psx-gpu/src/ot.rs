//! Ordering table: the PS1 has no depth buffer, so games bucket primitives by
//! a depth index (typically GTE `AVSZ3/4` → OTZ) and draw far buckets first.
//!
//! Mirrors the PsyQ `ClearOTagR` + `AddPrim` + `DrawOTag` convention:
//! higher indices are drawn first, and within one bucket the most recently
//! added primitive is drawn first (AddPrim links at the head).

use crate::prim::Prim;
use crate::Gpu;

#[derive(Clone, Debug)]
pub struct OrderingTable {
    buckets: Vec<Vec<Vec<u32>>>,
}

impl OrderingTable {
    pub fn new(len: usize) -> Self {
        OrderingTable { buckets: vec![Vec::new(); len.max(1)] }
    }

    pub fn len(&self) -> usize {
        self.buckets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buckets.iter().all(Vec::is_empty)
    }

    pub fn clear(&mut self) {
        self.buckets.iter_mut().for_each(Vec::clear);
    }

    /// Add at depth `z`, clamped into the table. Returns the bucket used.
    pub fn add(&mut self, z: i32, prim: &Prim) -> usize {
        let mut words = Vec::new();
        prim.encode(&mut words);
        self.add_words(z, words)
    }

    /// Add pre-encoded GP0 words (e.g. a packet captured from an emulator).
    pub fn add_words(&mut self, z: i32, words: Vec<u32>) -> usize {
        let i = z.clamp(0, self.buckets.len() as i32 - 1) as usize;
        self.buckets[i].push(words);
        i
    }

    /// Words in submission order, without drawing.
    pub fn linearize(&self) -> Vec<u32> {
        let mut out = Vec::new();
        for bucket in self.buckets.iter().rev() {
            for words in bucket.iter().rev() {
                out.extend_from_slice(words);
            }
        }
        out
    }

    pub fn draw(&self, gpu: &mut Gpu) {
        for bucket in self.buckets.iter().rev() {
            for words in bucket.iter().rev() {
                gpu.gp0_all(words);
            }
        }
    }
}
