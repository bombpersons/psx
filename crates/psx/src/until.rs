//! `run --until ADDR:FMT=VALUE`: stop the run at the first vblank where
//! samples hold given values, so a route waits on the game's state (a menu
//! up, a level loaded) rather than a fixed field.

use serde_json::{Value, json};

use crate::ram::{Fmt, Sample, Val, signed};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl Op {
    pub fn test<T: PartialOrd>(self, v: T, want: T) -> bool {
        match self {
            Op::Eq => v == want,
            Op::Ne => v != want,
            Op::Lt => v < want,
            Op::Le => v <= want,
            Op::Gt => v > want,
            Op::Ge => v >= want,
        }
    }
}

/// `NAME OP VALUE` split into its parts, OP one of = != < <= > >=.
pub fn split(s: &str) -> Result<(&str, Op, &str), String> {
    let bad = || format!("--until {s:?}: expected ADDR:FMT=VALUE (or != < <= > >=)");
    // Not a names path's `->`.
    let at = s.char_indices().find(|&(i, c)| "=!<>".contains(c) && !(c == '>' && s[..i].ends_with('-')));
    let at = at.ok_or_else(bad)?.0;
    let (sel, rest) = (s[..at].trim(), &s[at..]);
    let (op, value) = [("!=", Op::Ne), ("<=", Op::Le), (">=", Op::Ge), ("=", Op::Eq), ("<", Op::Lt), (">", Op::Gt)]
        .into_iter()
        .find_map(|(t, op)| rest.strip_prefix(t).map(|v| (op, v.trim())))
        .ok_or_else(bad)?;
    Ok((sel, op, value))
}

/// One condition: a sample column compared with a value in its format.
#[derive(Clone, Debug)]
pub struct Until {
    pub text: String,
    pub col: Sample,
    op: Op,
    want: Val,
}

/// A number in the order its format reads: signed and fixed point as signed.
fn ordered(fmt: Fmt, raw: u32) -> i64 {
    match fmt {
        Fmt::Signed(w) | Fmt::Fixed(w, _) => signed(raw, w as usize),
        Fmt::Unsigned(_) | Fmt::Hex(_) | Fmt::Bits(..) | Fmt::Str(_) => raw as i64,
    }
}

/// `ADDR:FMT OP VALUE`, OP one of = != < <= > >=; VALUE is written in the
/// format.
pub fn parse(s: &str) -> Result<Until, String> {
    let (sel, op, value) = split(s)?;
    let col = crate::ram::parse_sample(sel).map_err(|e| format!("--until {e}"))?;
    let want =
        col.fmt.given(value).ok_or_else(|| format!("--until {s:?}: {value:?} is not a {} value", col.fmt.name()))?;
    if matches!(want, Val::Bytes(_)) && !matches!(op, Op::Eq | Op::Ne) {
        return Err(format!("--until {s:?}: a byte string is equal or not (= or !=)"));
    }
    Ok(Until { text: s.trim().to_string(), want, col, op })
}

impl Until {
    /// Never while a pointer leads outside memory.
    pub fn holds(&self, m: &psx_core::Machine) -> bool {
        self.col.read_in(m).is_some_and(|v| match (&v, &self.want) {
            (&Val::Num(v), &Val::Num(w)) => self.op.test(ordered(self.col.fmt, v), ordered(self.col.fmt, w)),
            // A byte string: = or != only (parse refuses the others).
            (v, w) => (v == w) == (self.op == Op::Eq),
        })
    }
}

/// Whether each condition holds now, with its value (names only).
pub fn check(conds: &[Until], m: &psx_core::Machine) -> Value {
    let each: Vec<Value> =
        conds.iter().map(|c| json!({"condition": c.text, "holds": c.holds(m), "value": c.col.json_in(m)})).collect();
    json!({"met": each.iter().all(|c| c["holds"] == true), "conditions": each})
}

/// The report section: whether every condition held, where, and the values
/// at the run's end (names only).
pub fn report(conds: &[Until], met: Option<(u64, u64)>, m: &psx_core::Machine) -> Value {
    let values: serde_json::Map<_, _> = conds.iter().map(|c| (c.col.name.clone(), c.col.json_in(m))).collect();
    let mut v = json!({
        "conditions": conds.iter().map(|c| &c.text).collect::<Vec<_>>(),
        "met": met.is_some(),
        "values": values,
    });
    match met {
        Some((run_frame, frame)) => {
            v["run_frame"] = json!(run_frame);
            v["frame"] = json!(frame);
            v["note"] = json!(
                "the run stopped at this vblank, the first where every condition held; its files and state are from here: continue with from_state"
            );
        }
        None => v["note"] = json!("the conditions never all held: the run went its full frames"),
    }
    v
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn a_paths_arrow_is_not_a_comparison() {
        let (sel, op, v) = super::split("car_0->speed:i16>=-3").unwrap();
        assert_eq!((sel, op == super::Op::Ge, v), ("car_0->speed:i16", true, "-3"));
        assert_eq!(super::split("a->b>2").unwrap().0, "a->b");
    }

    #[test]
    fn conditions_compare_in_the_columns_format() {
        let mut m = psx_core::Machine::new(vec![0; 0x8_0000]);
        m.bus.ram[0x10000] = 3;
        m.bus.ram[0x10004..0x10006].copy_from_slice(&(-5i16).to_le_bytes());
        m.bus.ram[0x10008..0x1000C].copy_from_slice(&(3i32 << 15).to_le_bytes());
        let holds = |m: &psx_core::Machine, s: &str| parse(s).unwrap().holds(m);
        assert!(holds(&m, "80010000:u8=3") && holds(&m, "80010000:u8 = 0x3") && !holds(&m, "80010000:u8!=3"));
        assert!(holds(&m, "80010004:i16<0") && holds(&m, "80010004:i16<=-5") && !holds(&m, "80010004:i16>-5"));
        assert!(holds(&m, "80010008:fx16.16=1.5") && holds(&m, "80010008:fx16.16>1"));
        assert!(holds(&m, "80010004:u16=0xFFFB"));
        let conds = vec![parse("80010000:u8=3").unwrap(), parse("80010004:i16>0").unwrap()];
        let v = super::check(&conds, &m);
        assert_eq!((v["met"].clone(), v["conditions"][1]["value"].clone()), (false.into(), (-5).into()));
        let err = |s: &str| parse(s).unwrap_err();
        assert!(err("80010000:u8=1.5").contains("not a u8"));
        assert!(err("nothing=1").contains("nothing"));
        m.bus.ram[0x10020..0x10025].copy_from_slice(b"a=b\0\x01");
        assert!(holds(&m, r"80010020:str8=a=b\x00\x01") && !holds(&m, "80010020:str8=a=b"));
        // Registers, and memory through them.
        m.cpu.regs[7] = 0x8001_0000;
        m.cpu.regs[29] = 0x8000_FFFC;
        assert!(holds(&m, "a3=0x80010000") && holds(&m, "$a3:x32=0x80010000") && !holds(&m, "a3!=0x80010000"));
        assert!(holds(&m, "sp+4:u8=3") && holds(&m, "4(sp):u8=3") && holds(&m, "a3+4:i16=-5"));
        assert!(parse("80010020:str8<a").unwrap_err().contains("= or !="));
    }
}
