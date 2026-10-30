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
    let at = s.find(['=', '!', '<', '>']).ok_or_else(bad)?;
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
    pub fn holds(&self, ram: &[u8], scratch: &[u8]) -> bool {
        self.col.read(ram, scratch).is_some_and(|v| match (&v, &self.want) {
            (&Val::Num(v), &Val::Num(w)) => self.op.test(ordered(self.col.fmt, v), ordered(self.col.fmt, w)),
            // A byte string: = or != only (parse refuses the others).
            (v, w) => (v == w) == (self.op == Op::Eq),
        })
    }
}

/// Whether each condition holds now, with its value (names only).
pub fn check(conds: &[Until], ram: &[u8], scratch: &[u8]) -> Value {
    let each: Vec<Value> = conds
        .iter()
        .map(|c| json!({"condition": c.text, "holds": c.holds(ram, scratch), "value": c.col.json(ram, scratch)}))
        .collect();
    json!({"met": each.iter().all(|c| c["holds"] == true), "conditions": each})
}

/// The report section: whether every condition held, where, and the values
/// at the run's end (names only).
pub fn report(conds: &[Until], met: Option<(u64, u64)>, ram: &[u8], scratch: &[u8]) -> Value {
    let values: serde_json::Map<_, _> = conds.iter().map(|c| (c.col.name.clone(), c.col.json(ram, scratch))).collect();
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
    fn conditions_compare_in_the_columns_format() {
        let mut ram = vec![0u8; crate::ram::RAM_SIZE];
        let scratch = vec![0u8; crate::ram::SCRATCH_SIZE];
        ram[0x10000] = 3;
        ram[0x10004..0x10006].copy_from_slice(&(-5i16).to_le_bytes());
        ram[0x10008..0x1000C].copy_from_slice(&(3i32 << 15).to_le_bytes());
        let holds = |s: &str| parse(s).unwrap().holds(&ram, &scratch);
        assert!(holds("80010000:u8=3") && holds("80010000:u8 = 0x3") && !holds("80010000:u8!=3"));
        assert!(holds("80010004:i16<0") && holds("80010004:i16<=-5") && !holds("80010004:i16>-5"));
        assert!(holds("80010008:fx16.16=1.5") && holds("80010008:fx16.16>1"));
        assert!(holds("80010004:u16=0xFFFB"));
        let conds = vec![parse("80010000:u8=3").unwrap(), parse("80010004:i16>0").unwrap()];
        let v = super::check(&conds, &ram, &scratch);
        assert_eq!((v["met"].clone(), v["conditions"][1]["value"].clone()), (false.into(), (-5).into()));
        let err = |s: &str| parse(s).unwrap_err();
        assert!(err("80010000:u8=1.5").contains("not a u8"));
        assert!(err("nothing=1").contains("nothing"));
        ram[0x10020..0x10025].copy_from_slice(b"a=b\0\x01");
        let holds = |s: &str| parse(s).unwrap().holds(&ram, &scratch);
        assert!(holds(r"80010020:str8=a=b\x00\x01") && !holds("80010020:str8=a=b"));
        assert!(parse("80010020:str8<a").unwrap_err().contains("= or !="));
    }
}
