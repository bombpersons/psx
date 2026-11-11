//! A function's control flow, from the inventory's blocks: which blocks
//! follow which, what dominates what, the compiler's division checks, and
//! what each conditional branch tests.
use std::collections::BTreeMap;

/// `break 7` (division by zero) and `break 6` (overflow): GCC's division
/// checks trap with these.
const DIV_TRAPS: [u32; 2] = [0x0007_000D, 0x0006_000D];

/// One function's blocks and the edges between them.
pub(crate) struct Flow {
    /// Each block as `[start, end)`, by start.
    pub blocks: Vec<(u32, u32)>,
    pub succ: Vec<Vec<usize>>,
    pub pred: Vec<Vec<usize>>,
    /// Each block's immediate dominator (the entry's is itself); None when
    /// the graph does not reach it.
    pub idom: Vec<Option<usize>>,
    /// Blocks of GCC's division checks: the traps and the test between them.
    pub check_block: Vec<bool>,
    /// Branches of GCC's division checks, by address.
    pub check_branch: Vec<u32>,
}

/// What a conditional branch at `pc` (word `i`) does: its taken target and
/// whether taking it calls (bltzal, bgezal).
fn branch(pc: u32, i: u32) -> Option<(u32, bool)> {
    let op = i >> 26;
    let rt = (i >> 16) & 31;
    let ok = matches!(op, 0x04..=0x07) || (op == 0x01 && matches!(rt, 0x00 | 0x01 | 0x10 | 0x11));
    ok.then(|| (pc.wrapping_add(4).wrapping_add(((i & 0xFFFF) as i16 as i32 as u32) << 2), op == 0x01 && rt >= 0x10))
}

impl Flow {
    /// The flow of the function at `entry` whose blocks are `blocks`
    /// (`[start, end)`, by start), its code read through `word`.
    pub(crate) fn new(entry: u32, blocks: Vec<(u32, u32)>, word: impl Fn(u32) -> Option<u32>) -> Flow {
        let n = blocks.len();
        let at = |a: u32| -> Option<usize> {
            let i = blocks.partition_point(|b| b.0 <= a).checked_sub(1)?;
            (a < blocks[i].1).then_some(i)
        };
        let mut succ = vec![Vec::new(); n];
        let mut switches = Vec::new();
        for (b, &(s, e)) in blocks.iter().enumerate() {
            let last = e.checked_sub(8).filter(|&t| t >= s).and_then(|t| Some((t, word(t)?)));
            let mut to = Vec::new();
            match last {
                Some((t, i)) if branch(t, i).is_some() => {
                    let (target, call) = branch(t, i).unwrap();
                    let (rs, rt) = ((i >> 21) & 31, (i >> 16) & 31);
                    let always =
                        matches!(i >> 26, 0x04 | 0x06) && rs == rt || (i >> 26 == 0x01 && rs == 0 && rt & 1 == 1);
                    let never = i >> 26 == 0x05 && rs == rt;
                    if !call && !never {
                        to.extend(at(target));
                    }
                    if !always {
                        to.extend(at(e));
                    }
                }
                // j: on to its target in the function (else a tail call).
                Some((t, i)) if i >> 26 == 0x02 => to.extend(at((t & 0xF000_0000) | ((i & 0x03FF_FFFF) << 2))),
                // jr: a return, or a switch whose cases are found below.
                Some((_, i)) if i & 0xFC1F_FFFF == 8 => {
                    if i >> 21 != 31 {
                        switches.push(b);
                    }
                }
                _ => to.extend(at(e)),
            }
            to.dedup();
            succ[b] = to;
        }
        let entry = at(entry).unwrap_or(0);
        let mut pred = vec![Vec::new(); n];
        for (b, s) in succ.iter().enumerate() {
            for &t in s {
                pred[t].push(b);
            }
        }
        // A block nothing leads to is a switch case: the switches lead to it.
        if !switches.is_empty() {
            let cases: Vec<usize> = (0..n).filter(|&b| b != entry && pred[b].is_empty()).collect();
            for b in cases {
                for &w in &switches {
                    succ[w].push(b);
                    pred[b].push(w);
                }
            }
        }
        let idom = dominators(entry, &succ, &pred);
        let mut f = Flow { blocks, succ, pred, idom, check_block: vec![false; n], check_branch: Vec::new() };
        f.division_checks(&word);
        f
    }

    /// The block holding `a`.
    pub(crate) fn block(&self, a: u32) -> Option<usize> {
        let i = self.blocks.partition_point(|b| b.0 <= a).checked_sub(1)?;
        (a < self.blocks[i].1).then_some(i)
    }

    /// The conditional branch ending block `b`, with its word.
    fn branch_of(&self, b: usize, word: &impl Fn(u32) -> Option<u32>) -> Option<(u32, u32)> {
        let (s, e) = self.blocks[b];
        let t = e.checked_sub(8).filter(|&t| t >= s)?;
        let i = word(t)?;
        branch(t, i).map(|_| (t, i))
    }

    /// GCC guards each division: `bnez b` over a `break 7`, then for a
    /// signed one `b == -1` and `a == 0x80000000` both going to a `break 6`.
    /// Those branches and blocks are the compiler's, never a gap.
    fn division_checks(&mut self, word: &impl Fn(u32) -> Option<u32>) {
        let trap = |b: Option<usize>| b.is_some_and(|b| word(self.blocks[b].0).is_some_and(|i| DIV_TRAPS.contains(&i)));
        for b in 0..self.blocks.len() {
            if trap(Some(b)) {
                self.check_block[b] = true;
            }
        }
        let mut checks = Vec::new();
        for b in 0..self.blocks.len() {
            let Some((pc, i)) = self.branch_of(b, word) else { continue };
            let (target, _) = branch(pc, i).unwrap();
            let (taken, fell) = (self.block(target), self.block(pc + 8));
            // The way on from here tests again with the same target, and
            // its other way traps.
            let chain = |w: Option<usize>| {
                w.and_then(|w| self.branch_of(w, word))
                    .is_some_and(|(p, j)| branch(p, j).is_some_and(|(t, _)| t == target) && trap(self.block(p + 8)))
            };
            if trap(taken) || trap(fell) || chain(fell) {
                checks.push((b, pc));
            }
        }
        for (b, pc) in checks {
            self.check_branch.push(pc);
            // A block that is only the test and its slot.
            if self.blocks[b].1 - self.blocks[b].0 == 8 {
                self.check_block[b] = true;
            }
        }
    }

    /// Whether `d` is dominated by `by`.
    pub(crate) fn dominates(&self, by: usize, mut d: usize) -> bool {
        loop {
            if d == by {
                return true;
            }
            match self.idom[d] {
                Some(i) if i != d => d = i,
                _ => return false,
            }
        }
    }

    /// The block only the edge `from` to `to` enters, when there is one:
    /// `to` when that edge is its only way in.
    pub(crate) fn only_through(&self, from: usize, to: usize) -> Option<usize> {
        (self.pred[to] == [from] && self.succ[from].iter().filter(|&&s| s == to).count() == 1).then_some(to)
    }

    /// The blocks `to` dominates, the compiler's checks left out.
    pub(crate) fn behind(&self, to: usize) -> Vec<usize> {
        (0..self.blocks.len()).filter(|&d| !self.check_block[d] && self.dominates(to, d)).collect()
    }
}

/// Immediate dominators (Cooper, Harvey and Kennedy's iteration).
fn dominators(entry: usize, succ: &[Vec<usize>], pred: &[Vec<usize>]) -> Vec<Option<usize>> {
    let n = succ.len();
    if n == 0 {
        return Vec::new();
    }
    // Reverse postorder from the entry.
    let mut order = Vec::new();
    let mut seen = vec![false; n];
    let mut stack = vec![(entry, 0usize)];
    seen[entry] = true;
    while let Some((b, k)) = stack.pop() {
        if let Some(&s) = succ[b].get(k) {
            stack.push((b, k + 1));
            if !seen[s] {
                seen[s] = true;
                stack.push((s, 0));
            }
        } else {
            order.push(b);
        }
    }
    order.reverse();
    let mut rank = vec![usize::MAX; n];
    for (r, &b) in order.iter().enumerate() {
        rank[b] = r;
    }
    let mut idom: Vec<Option<usize>> = vec![None; n];
    idom[entry] = Some(entry);
    let mut changed = true;
    while changed {
        changed = false;
        for &b in order.iter().skip(1) {
            let mut new: Option<usize> = None;
            for &p in &pred[b] {
                if idom[p].is_none() {
                    continue;
                }
                new = Some(match new {
                    None => p,
                    Some(mut x) => {
                        let mut y = p;
                        while x != y {
                            while rank[x] > rank[y] {
                                x = idom[x].unwrap();
                            }
                            while rank[y] > rank[x] {
                                y = idom[y].unwrap();
                            }
                        }
                        x
                    }
                });
            }
            if new.is_some() && idom[b] != new {
                idom[b] = new;
                changed = true;
            }
        }
    }
    idom
}

/// What a conditional branch tests, read back through the code before it:
/// `[0x8007529C] == 2`, `([0x800751F0] & 0x10) == 0`, `0x80054DE8() == 0`.
/// `taken` says whether taking the branch means the test holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Test {
    pub key: String,
    pub taken: bool,
}

/// A value read back: a constant, or an expression of memory and calls.
#[derive(Clone)]
enum Val {
    Const(u32),
    Expr(String),
}

impl Val {
    fn text(&self) -> String {
        match self {
            Val::Const(c) => number(*c),
            Val::Expr(e) => e.clone(),
        }
    }
}

fn number(c: u32) -> String {
    let s = c as i32;
    if (-4096..4096).contains(&s) { s.to_string() } else { format!("0x{c:X}") }
}

/// Reads what branches test, in one function's flow.
pub(crate) struct Tests<'a, W: Fn(u32) -> Option<u32>> {
    pub flow: &'a Flow,
    pub word: W,
    pub gp: Option<u32>,
    pub entry: u32,
    /// Set while reading a test that names an argument or a saved
    /// register: what it means holds only in this function.
    pub local: std::cell::Cell<bool>,
}

/// The saved registers (s0-s7, fp): a function keeps a variable there.
fn saved(r: u32) -> bool {
    matches!(r, 16..=23 | 30)
}

const REGS: [&str; 32] = [
    "zero", "at", "v0", "v1", "a0", "a1", "a2", "a3", "t0", "t1", "t2", "t3", "t4", "t5", "t6", "t7", "s0", "s1", "s2",
    "s3", "s4", "s5", "s6", "s7", "t8", "t9", "k0", "k1", "gp", "sp", "fp", "ra",
];

/// The register instruction `i` writes, when it is a plain computation or
/// load (not a call, a move from hi/lo or a coprocessor's).
fn dest(i: u32) -> Option<u32> {
    let (op, rt, rd) = (i >> 26, (i >> 16) & 31, (i >> 11) & 31);
    match op {
        0 if !matches!(i & 0x3F, 0x08 | 0x09 | 0x0C | 0x0D | 0x18..=0x1B | 0x11 | 0x13) => Some(rd),
        0x08..=0x0F | 0x20..=0x26 => Some(rt),
        _ => None,
    }
}

impl<W: Fn(u32) -> Option<u32>> Tests<'_, W> {
    /// The test of the conditional branch at `pc`.
    pub(crate) fn test(&self, pc: u32) -> Option<Test> {
        self.local.set(false);
        let i = (self.word)(pc)?;
        let b = self.flow.block(pc)?;
        let (rs, rt) = ((i >> 21) & 31, (i >> 16) & 31);
        let v = |r: u32| self.value(r, b, pc, 6);
        let (key, taken) = match i >> 26 {
            0x04 | 0x05 => {
                let (mut x, mut y) = (v(rs)?, if rt == 0 { Val::Const(0) } else { v(rt)? });
                if matches!(x, Val::Const(_)) {
                    std::mem::swap(&mut x, &mut y);
                }
                let later = matches!((&x, &y), (Val::Expr(a), Val::Expr(c)) if a > c);
                if later {
                    std::mem::swap(&mut x, &mut y);
                }
                (format!("{} == {}", x.text(), y.text()), i >> 26 == 0x04)
            }
            0x06 | 0x07 => (format!("{} > 0", v(rs)?.text()), i >> 26 == 0x07),
            0x01 => (format!("{} < 0", v(rs)?.text()), rt & 1 == 0),
            _ => return None,
        };
        let key = if self.local.get() { format!("{key} in 0x{:08X}", self.entry) } else { key };
        Some(Test { key, taken })
    }

    /// A register read back to where the code before does not say: an
    /// argument at the entry, a saved register's one value in the function,
    /// or the saved register by name.
    fn unknown(&self, r: u32, at_entry: bool, depth: u32) -> Option<Val> {
        if at_entry && (4..=7).contains(&r) {
            self.local.set(true);
            return Some(Val::Expr(format!("arg{}", r - 4)));
        }
        if !saved(r) {
            return None;
        }
        // Its one writer in the function, but the epilogue's restore.
        let mut writers = self.flow.blocks.iter().enumerate().flat_map(|(b, &(s, e))| {
            (s..e).step_by(4).filter_map(move |a| {
                let i = (self.word)(a)?;
                (dest(i) == Some(r) && !(i >> 26 == 0x23 && (i >> 21) & 31 == 29)).then_some((b, a))
            })
        });
        let one = match (writers.next(), writers.next()) {
            (Some((b, a)), None) => self.value(r, b, a + 4, depth.saturating_sub(1)),
            _ => None,
        };
        if one.is_some() {
            return one;
        }
        self.local.set(true);
        Some(Val::Expr(REGS[r as usize].into()))
    }

    /// The value register `r` holds just before `at`, read back from `at`
    /// in block `b` (and on into its one way in).
    fn value(&self, r: u32, mut b: usize, mut at: u32, depth: u32) -> Option<Val> {
        if r == 0 {
            return Some(Val::Const(0));
        }
        if depth == 0 {
            return None;
        }
        for _ in 0..64 {
            if at <= self.flow.blocks[b].0 {
                match self.flow.pred[b].as_slice() {
                    [p] if *p != b => {
                        b = *p;
                        at = self.flow.blocks[b].1;
                    }
                    [] => return self.unknown(r, self.flow.blocks[b].0 == self.entry, depth),
                    _ => return self.unknown(r, false, depth),
                }
                continue;
            }
            at -= 4;
            let i = (self.word)(at)?;
            let (op, rs, rt, rd) = (i >> 26, (i >> 21) & 31, (i >> 16) & 31, (i >> 11) & 31);
            let imm = i & 0xFFFF;
            let simm = imm as i16 as i32 as u32;
            // A call: v0 and v1 are what it returns, the other registers
            // the callee may change are unknown.
            if op == 0x03 || (op == 0 && i & 0x3F == 0x09) {
                return match r {
                    2 | 3 if op == 0x03 => {
                        Some(Val::Expr(format!("0x{:08X}()", (at & 0xF000_0000) | ((i & 0x03FF_FFFF) << 2))))
                    }
                    1..=15 | 24 | 25 | 31 => None,
                    _ => continue,
                };
            }
            if dest(i) != Some(r) {
                // Written some other way (a move from hi/lo, a coprocessor):
                // what it holds is not read here.
                let other = match op {
                    0 => matches!(i & 0x3F, 0x10 | 0x12) && rd == r,
                    0x10..=0x13 => (i >> 21) & 31 == 0 && rt == r,
                    _ => false,
                };
                if other {
                    return None;
                }
                continue;
            }
            let v = |r: u32| self.value(r, b, at, depth - 1);
            let expr = |s: String| Some(Val::Expr(s));
            return match op {
                0x20..=0x26 => {
                    let addr = match (rs, self.gp) {
                        (0, _) => Some(simm),
                        (28, Some(gp)) => Some(gp.wrapping_add(simm)),
                        _ => match v(rs)? {
                            Val::Const(c) => Some(c.wrapping_add(simm)),
                            Val::Expr(e) if simm == 0 => return expr(format!("[{e}]")),
                            Val::Expr(e) => return expr(format!("[{e} + {}]", number(simm))),
                        },
                    }?;
                    expr(format!("[0x{addr:08X}]"))
                }
                0x0F => Some(Val::Const(imm << 16)),
                0x08 | 0x09 => match v(rs)? {
                    Val::Const(c) => Some(Val::Const(c.wrapping_add(simm))),
                    Val::Expr(e) if (simm as i32) < 0 => expr(format!("({e} - {})", number(simm.wrapping_neg()))),
                    Val::Expr(e) => expr(format!("({e} + {})", number(simm))),
                },
                0x0D => match v(rs)? {
                    Val::Const(c) => Some(Val::Const(c | imm)),
                    Val::Expr(e) => expr(format!("({e} | 0x{imm:X})")),
                },
                0x0C => expr(format!("({} & 0x{imm:X})", v(rs)?.text())),
                0x0E => expr(format!("({} ^ 0x{imm:X})", v(rs)?.text())),
                0x0A | 0x0B => expr(format!("({} < {})", v(rs)?.text(), number(simm))),
                0 => {
                    let sa = (i >> 6) & 31;
                    match i & 0x3F {
                        0x21 | 0x25 if rt == 0 => v(rs),
                        0x21 | 0x25 if rs == 0 => v(rt),
                        0x21 => expr(format!("({} + {})", v(rs)?.text(), v(rt)?.text())),
                        0x23 => expr(format!("({} - {})", v(rs)?.text(), v(rt)?.text())),
                        0x24 => expr(format!("({} & {})", v(rs)?.text(), v(rt)?.text())),
                        0x25 => expr(format!("({} | {})", v(rs)?.text(), v(rt)?.text())),
                        0x2A | 0x2B => expr(format!("({} < {})", v(rs)?.text(), v(rt)?.text())),
                        0x00 => expr(format!("({} << {sa})", v(rt)?.text())),
                        0x02 | 0x03 => expr(format!("({} >> {sa})", v(rt)?.text())),
                        _ => None,
                    }
                }
                _ => None,
            };
        }
        None
    }
}

/// A function's flow over an image's words, by the branch addresses that
/// matter for its gaps: each conditional branch's test, and for each
/// branch no run reached, the branch whose missed way alone leads to it.
pub(crate) struct Gaps {
    /// The branches of the compiler's division checks.
    pub checks: Vec<u32>,
    /// What each branch tests, when the code before it says.
    pub tests: BTreeMap<u32, Test>,
    /// For each branch behind another's missed way: that branch.
    pub behind: BTreeMap<u32, u32>,
    /// For each branch with a missed way: the blocks only that way reaches.
    pub blocks_behind: BTreeMap<u32, usize>,
}

/// The gaps of one function: `branches` are its conditional branches with
/// the ways runs took (1 taken, 2 fell through; 0 never ran).
pub(crate) fn gaps(
    flow: &Flow,
    entry: u32,
    gp: Option<u32>,
    word: impl Fn(u32) -> Option<u32>,
    branches: &[(u32, u8)],
) -> Gaps {
    let tests = Tests { flow, word: &word, gp, entry, local: Default::default() };
    let mut out = Gaps {
        checks: flow.check_branch.clone(),
        tests: BTreeMap::new(),
        behind: BTreeMap::new(),
        blocks_behind: BTreeMap::new(),
    };
    // The block each missed way alone enters, and its branch.
    let mut frontier: BTreeMap<usize, u32> = BTreeMap::new();
    for &(pc, ways) in branches {
        if let Some(t) = tests.test(pc) {
            out.tests.insert(pc, t);
        }
        if ways == 0 || ways == 3 || flow.check_branch.contains(&pc) {
            continue;
        }
        let (Some(from), Some(i)) = (flow.block(pc), word(pc)) else { continue };
        let Some((target, _)) = branch(pc, i) else { continue };
        let missed = if ways == 1 { pc + 8 } else { target };
        let to = flow.block(missed).and_then(|to| flow.only_through(from, to));
        let n = to.map_or(0, |to| flow.behind(to).len());
        out.blocks_behind.insert(pc, n);
        if let Some(to) = to {
            frontier.insert(to, pc);
        }
    }
    for &(pc, ways) in branches {
        if ways != 0 {
            continue;
        }
        let Some(mut b) = flow.block(pc) else { continue };
        loop {
            if let Some(&f) = frontier.get(&b) {
                out.behind.insert(pc, f);
                break;
            }
            match flow.idom[b] {
                Some(i) if i != b => b = i,
                _ => break,
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Words from `base`, read as code.
    fn code(base: u32, words: &[u32]) -> impl Fn(u32) -> Option<u32> + '_ {
        move |a| words.get(a.checked_sub(base)? as usize / 4).copied()
    }

    fn spans(starts: &[u32], end: u32) -> Vec<(u32, u32)> {
        starts.iter().enumerate().map(|(k, &s)| (s, starts.get(k + 1).copied().unwrap_or(end))).collect()
    }

    #[test]
    fn a_division_check_is_the_compilers() {
        // 0: div a0,a1; 4: bnez a1,+2 (to 0x10); 8: nop; C: break 7;
        // 10: li at,-1; 14: bne a1,at,+4 (to 0x28); 18: lui at,0x8000;
        // 1C: bne a0,at,+2 (to 0x28); 20: nop; 24: break 6; 28: mflo v0; 2C: jr ra; 30: nop
        let w = [
            0x0085_001A,
            0x14A0_0002,
            0,
            0x0007_000D,
            0x2401_FFFF,
            0x14A1_0004,
            0x3C01_8000,
            0x1481_0002,
            0,
            0x0006_000D,
            0x0000_1012,
            0x03E0_0008,
            0,
        ];
        let base = 0x8001_0000;
        let f =
            Flow::new(base, spans(&[0, 0xC, 0x10, 0x1C, 0x24, 0x28].map(|o| base + o), base + 0x34), code(base, &w));
        assert_eq!(f.check_branch, [base + 4, base + 0x14, base + 0x1C]);
        assert_eq!(f.check_block, [false, true, false, true, true, false]);
    }

    #[test]
    fn a_way_never_taken_answers_for_the_code_only_it_reaches() {
        // 0: lw v0,0x529C(gp)  4: addiu v1,zero,2  8: bne v0,v1,+3 (to 0x18)  C: nop
        // 10: beq a0,zero,+1 (to 0x18)  14: nop  18: jr ra  1C: nop
        let w = [0x8F82_529C, 0x2403_0002, 0x1443_0003, 0, 0x1080_0001, 0, 0x03E0_0008, 0];
        let base = 0x8001_0000;
        let word = code(base, &w);
        let f = Flow::new(base, spans(&[0, 0x10, 0x18].map(|o| base + o), base + 0x20), &word);
        // Ran, never fell through; the branch behind it never ran.
        let g = gaps(&f, base, Some(0x8007_0000), &word, &[(base + 8, 1), (base + 0x10, 0)]);
        assert_eq!(g.tests[&(base + 8)], Test { key: "[0x8007529C] == 2".into(), taken: false });
        assert_eq!(g.tests[&(base + 0x10)].key, "arg0 == 0 in 0x80010000");
        assert_eq!(g.behind[&(base + 0x10)], base + 8);
        assert_eq!(g.blocks_behind[&(base + 8)], 1);
    }

    #[test]
    fn a_call_result_and_a_mask_are_read_back() {
        // 0: jal 0x80012340  4: nop  8: andi v0,v0,0x10  C: beq v0,zero,+1  10: nop  14: jr ra  18: nop
        let w = [0x0C00_48D0, 0, 0x3042_0010, 0x1040_0001, 0, 0x03E0_0008, 0];
        let base = 0x8001_0000;
        let word = code(base, &w);
        let f = Flow::new(base, spans(&[0, 0x14].map(|o| base + o), base + 0x1C), &word);
        let t = Tests { flow: &f, word: &word, gp: None, entry: base, local: Default::default() };
        assert_eq!(t.test(base + 0xC), Some(Test { key: "(0x80012340() & 0x10) == 0".into(), taken: true }));
    }
}
