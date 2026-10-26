//! `run --until NAME=VALUE`: stop the run at the first vblank where named
//! samples hold given values, so a route waits on the game's state (a menu
//! up, a level loaded) rather than a fixed field.

use serde_json::{Value, json};

use crate::fieldmap::FieldMap;
use crate::ram::{Fmt, Sample, signed};

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
    let bad = || format!("--until {s:?}: expected NAME=VALUE (or != < <= > >=)");
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
    want: i64,
}

/// A value in the order its format reads: signed and fixed point as signed.
fn ordered(fmt: Fmt, raw: u32) -> i64 {
    match fmt {
        Fmt::Signed(w) | Fmt::Fixed(w, _) => signed(raw, w as usize),
        Fmt::Unsigned(_) | Fmt::Hex(_) | Fmt::Bits(..) => raw as i64,
    }
}

/// `NAME OP VALUE`, OP one of = != < <= > >=. NAME is one field map column
/// (a global or SUBJECT.FIELD) or ADDR:FMT; VALUE is written in its format.
pub fn parse(s: &str, map: Option<&FieldMap>) -> Result<Until, String> {
    let (sel, op, value) = split(s)?;
    let mut cols = crate::fieldmap::select(sel, map).map_err(|e| format!("--until {e}"))?;
    if cols.len() != 1 {
        return Err(format!("--until {s:?}: {sel} names {} fields; name one (SUBJECT.FIELD)", cols.len()));
    }
    let col = cols.remove(0);
    let raw =
        col.fmt.encode(value).ok_or_else(|| format!("--until {s:?}: {value:?} is not a {} value", col.fmt.name()))?;
    Ok(Until { text: s.trim().to_string(), want: ordered(col.fmt, raw), col, op })
}

impl Until {
    /// Never while a pointer leads outside memory.
    pub fn holds(&self, ram: &[u8], scratch: &[u8]) -> bool {
        self.col.read(ram, scratch).is_some_and(|v| self.op.test(ordered(self.col.fmt, v), self.want))
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
    use crate::fieldmap::FieldMap;

    #[test]
    fn conditions_compare_in_the_columns_format() {
        let map = FieldMap::parse(
            "[globals]\nmenu = \"80010000:u8\"\nx = \"80010004:i16\"\n[structs.p]\ny = \"0:fx16.16\"\nz = \"4:i32\"\n[subjects.lara]\nstruct = \"p\"\nat = \"80010008\"\n",
            "samples.toml".into(),
        )
        .unwrap();
        let mut ram = vec![0u8; crate::ram::RAM_SIZE];
        let scratch = vec![0u8; crate::ram::SCRATCH_SIZE];
        ram[0x10000] = 3;
        ram[0x10004..0x10006].copy_from_slice(&(-5i16).to_le_bytes());
        ram[0x10008..0x1000C].copy_from_slice(&(3i32 << 15).to_le_bytes());
        let holds = |s: &str| parse(s, Some(&map)).unwrap().holds(&ram, &scratch);
        assert!(holds("menu=3") && holds("menu = 0x3") && !holds("menu!=3"));
        assert!(holds("x<0") && holds("x<=-5") && !holds("x>-5") && holds("x>=-5"));
        assert!(holds("lara.y=1.5") && holds("lara.y>1"));
        assert!(holds("80010000:u8=3") && holds("80010004:u16=0xFFFB"));
        let conds = vec![parse("menu=3", Some(&map)).unwrap(), parse("x>0", Some(&map)).unwrap()];
        let v = super::check(&conds, &ram, &scratch);
        assert_eq!((v["met"].clone(), v["conditions"][1]["value"].clone()), (false.into(), (-5).into()));
        let err = |s: &str| parse(s, Some(&map)).unwrap_err();
        assert!(err("lara=1").contains("names 2 fields"));
        assert!(err("menu").contains("NAME=VALUE"));
        assert!(err("menu=1.5").contains("not a u8"));
        assert!(err("nothing=1").contains("nothing"));
    }
}
