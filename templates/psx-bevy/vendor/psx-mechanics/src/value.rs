//! Value types and raw values.
//!
//! Every state field, input, param and interface field has a type, and every
//! value is carried exactly as the original holds it: integers, fixed-point
//! numbers as their underlying integer, floats as their bit pattern.

use std::borrow::Cow;
use std::collections::BTreeMap;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};
use serde_json::json;

/// A field's declared type, as written in `mechanic.toml` and interfaces.
/// Which keys apply depends on `type`; [`Field::ty`] checks them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(transform = field_rules)]
pub struct Field {
    #[serde(rename = "type")]
    pub kind: Kind,
    /// `int`: width in bits (1 to 64). `float`: 32, 64 or 80.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bits: Option<u32>,
    /// `int`, `fixed`: whether the value is two's complement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signed: Option<bool>,
    /// `fixed`: integer bits, the sign bit included.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub int_bits: Option<u32>,
    /// `fixed`: fraction bits; the raw value is the real value × 2^frac_bits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frac_bits: Option<u32>,
    /// `enum`: the variants' names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variants: Option<Vec<String>>,
    /// `table`, `list`: the element type (a table's is not itself a table).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub of: Option<Box<Field>>,
    /// `table`: the number of elements.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub len: Option<u32>,
    /// `record`: its fields, by name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fields: Option<BTreeMap<String, Field>>,
    /// Documentation only: `"units"`, `"units/tick"`, `"angle/4096"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Bool,
    Int,
    Fixed,
    Enum,
    Float,
    /// Params only: a fixed-length list of one element type.
    Table,
    /// Params only: a list of one element type, of any length.
    List,
    /// Params only: named fields, each of its own type.
    Record,
}

/// The keys each kind needs, for the schema (the same rules as [`Field::ty`]).
fn field_rules(schema: &mut Schema) {
    let need = |kind: &str, keys: &[&str]| json!({"if": {"properties": {"type": {"const": kind}}}, "then": {"required": keys}});
    schema.insert(
        "allOf".into(),
        json!([
            need("int", &["bits", "signed"]),
            need("fixed", &["signed", "int_bits", "frac_bits"]),
            need("enum", &["variants"]),
            need("float", &["bits"]),
            need("table", &["of", "len"]),
            need("list", &["of"]),
            need("record", &["fields"]),
        ]),
    );
}

/// A checked type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Type {
    Bool,
    Int {
        bits: u32,
        signed: bool,
    },
    Fixed {
        signed: bool,
        int_bits: u32,
        frac_bits: u32,
    },
    Enum(Vec<String>),
    /// Carried as its bit pattern.
    Float {
        bits: u32,
    },
    Table {
        of: Box<Type>,
        len: u32,
    },
    List {
        of: Box<Type>,
    },
    Record(BTreeMap<String, Type>),
}

impl Field {
    /// The checked type, or what is wrong with the declaration.
    pub fn ty(&self) -> Result<Type, String> {
        let given = [
            ("bits", self.bits.is_some()),
            ("signed", self.signed.is_some()),
            ("int_bits", self.int_bits.is_some()),
            ("frac_bits", self.frac_bits.is_some()),
            ("variants", self.variants.is_some()),
            ("of", self.of.is_some()),
            ("len", self.len.is_some()),
            ("fields", self.fields.is_some()),
        ];
        let allowed: &[&str] = match self.kind {
            Kind::Bool => &[],
            Kind::Int => &["bits", "signed"],
            Kind::Fixed => &["signed", "int_bits", "frac_bits"],
            Kind::Enum => &["variants"],
            Kind::Float => &["bits"],
            Kind::Table => &["of", "len"],
            Kind::List => &["of"],
            Kind::Record => &["fields"],
        };
        let kind = format!("{:?}", self.kind).to_lowercase();
        if let Some((k, _)) = given.iter().find(|(k, g)| *g && !allowed.contains(k)) {
            return Err(format!("{k} does not apply to {kind}"));
        }
        if let Some(k) = allowed.iter().find(|k| !given.iter().any(|(g, set)| g == *k && *set)) {
            return Err(format!("{kind} needs {k}"));
        }
        Ok(match self.kind {
            Kind::Bool => Type::Bool,
            Kind::Int => {
                let bits = self.bits.unwrap();
                if !(1..=64).contains(&bits) {
                    return Err(format!("int bits {bits}: 1 to 64"));
                }
                Type::Int { bits, signed: self.signed.unwrap() }
            }
            Kind::Fixed => {
                let (signed, int_bits, frac_bits) =
                    (self.signed.unwrap(), self.int_bits.unwrap(), self.frac_bits.unwrap());
                if !(1..=64).contains(&(int_bits + frac_bits)) {
                    return Err(format!("fixed width {}: 1 to 64 bits", int_bits + frac_bits));
                }
                if signed && int_bits == 0 {
                    return Err("a signed fixed needs at least one integer bit (the sign)".into());
                }
                Type::Fixed { signed, int_bits, frac_bits }
            }
            Kind::Enum => {
                let v = self.variants.clone().unwrap();
                if v.is_empty() {
                    return Err("enum needs at least one variant".into());
                }
                if let Some(bad) = v.iter().find(|n| !is_name(n)) {
                    return Err(format!("variant {bad:?} is not a name ([a-z][a-z0-9_]*)"));
                }
                if let Some(dup) = v.iter().enumerate().find(|(i, n)| v[..*i].contains(n)) {
                    return Err(format!("variant {} is listed twice", dup.1));
                }
                Type::Enum(v)
            }
            Kind::Float => {
                let bits = self.bits.unwrap();
                if ![32, 64, 80].contains(&bits) {
                    return Err(format!("float bits {bits}: 32, 64 or 80"));
                }
                Type::Float { bits }
            }
            Kind::Table => {
                let of = self.of.as_ref().unwrap().ty().map_err(|e| format!("table element: {e}"))?;
                if matches!(of, Type::Table { .. }) {
                    return Err("a table's element cannot be a table".into());
                }
                let len = self.len.unwrap();
                if len == 0 {
                    return Err("a table needs at least one element".into());
                }
                Type::Table { of: Box::new(of), len }
            }
            Kind::List => {
                let of = self.of.as_ref().unwrap().ty().map_err(|e| format!("list element: {e}"))?;
                Type::List { of: Box::new(of) }
            }
            Kind::Record => {
                let fields = self.fields.as_ref().unwrap();
                if fields.is_empty() {
                    return Err("a record needs at least one field".into());
                }
                let mut out = BTreeMap::new();
                for (n, f) in fields {
                    if !is_name(n) {
                        return Err(format!("record field {n:?} is not a name ([a-z][a-z0-9_]*)"));
                    }
                    out.insert(n.clone(), f.ty().map_err(|e| format!("record field {n}: {e}"))?);
                }
                Type::Record(out)
            }
        })
    }
}

impl Type {
    /// A table, list or record: for params only.
    pub fn is_data(&self) -> bool {
        matches!(self, Type::Table { .. } | Type::List { .. } | Type::Record(_))
    }

    /// Whether `v` is a value of this type, or why not.
    pub fn check(&self, v: &Value) -> Result<(), String> {
        let range = |lo: i128, hi: i128| match v {
            Value::Int(n) if (lo..=hi).contains(n) => Ok(()),
            Value::Int(n) => Err(format!("{n} is outside {lo}..={hi}")),
            other => Err(format!("expected an integer, got {}", other.describe())),
        };
        let ints = |bits: u32, signed: bool| {
            if signed {
                range(-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1)
            } else {
                range(0, (1i128 << bits) - 1)
            }
        };
        match self {
            Type::Bool => match v {
                Value::Bool(_) => Ok(()),
                other => Err(format!("expected true or false, got {}", other.describe())),
            },
            Type::Int { bits, signed } => ints(*bits, *signed),
            Type::Fixed { signed, int_bits, frac_bits } => ints(int_bits + frac_bits, *signed),
            Type::Float { bits } => ints(*bits, false),
            Type::Enum(variants) => match v {
                Value::Variant(n) if variants.contains(n) => Ok(()),
                Value::Variant(n) => Err(format!("{n} is not one of {}", variants.join(", "))),
                other => Err(format!("expected a variant name, got {}", other.describe())),
            },
            Type::Table { of, len } => match v {
                Value::List(items) if items.len() == *len as usize => {
                    items.iter().enumerate().try_for_each(|(i, x)| of.check(x).map_err(|e| format!("[{i}]: {e}")))
                }
                Value::List(items) => Err(format!("expected {len} elements, got {}", items.len())),
                other => Err(format!("expected a list, got {}", other.describe())),
            },
            Type::List { of } => match v {
                Value::List(items) => {
                    items.iter().enumerate().try_for_each(|(i, x)| of.check(x).map_err(|e| format!("[{i}]: {e}")))
                }
                other => Err(format!("expected a list, got {}", other.describe())),
            },
            Type::Record(fields) => match v {
                Value::Record(r) => {
                    if let Some(n) = r.keys().find(|n| !fields.contains_key(*n)) {
                        return Err(format!("{n} is not a field"));
                    }
                    fields.iter().try_for_each(|(n, t)| match r.get(n) {
                        Some(x) => t.check(x).map_err(|e| format!(".{n}: {e}")),
                        None => Err(format!("{n} is missing")),
                    })
                }
                other => Err(format!("expected a record, got {}", other.describe())),
            },
        }
    }
}

/// A raw value: `true`/`false`; an integer (`int`, a `fixed`'s underlying
/// integer, a `float`'s bit pattern), written as a decimal string outside
/// ±2^53 in JSON; a variant's name; a table's or list's elements; or a
/// record's fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Bool(bool),
    Int(i128),
    Variant(String),
    List(Vec<Value>),
    Record(BTreeMap<String, Value>),
}

/// The largest integer JSON readers keep exactly.
const SAFE: i128 = 1 << 53;

impl Value {
    fn describe(&self) -> String {
        match self {
            Value::Bool(b) => b.to_string(),
            Value::Int(n) => n.to_string(),
            Value::Variant(n) => format!("{n:?}"),
            Value::List(l) => format!("a list of {}", l.len()),
            Value::Record(_) => "a record".into(),
        }
    }
}

impl Serialize for Value {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Value::Bool(b) => s.serialize_bool(*b),
            Value::Int(n) if (-SAFE..=SAFE).contains(n) => s.serialize_i64(*n as i64),
            Value::Int(n) => s.serialize_str(&n.to_string()),
            Value::Variant(n) => s.serialize_str(n),
            Value::List(l) => l.serialize(s),
            Value::Record(r) => r.serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for Value {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = Value;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("true/false, an integer, a decimal string, a variant name, a list or a record")
            }
            fn visit_bool<E>(self, b: bool) -> Result<Value, E> {
                Ok(Value::Bool(b))
            }
            fn visit_i64<E>(self, n: i64) -> Result<Value, E> {
                Ok(Value::Int(n.into()))
            }
            fn visit_u64<E>(self, n: u64) -> Result<Value, E> {
                Ok(Value::Int(n.into()))
            }
            fn visit_f64<E: serde::de::Error>(self, n: f64) -> Result<Value, E> {
                Err(E::custom(format!("{n}: raw values are integers (a float is its bit pattern)")))
            }
            fn visit_str<E: serde::de::Error>(self, s: &str) -> Result<Value, E> {
                if is_name(s) {
                    Ok(Value::Variant(s.into()))
                } else {
                    s.parse()
                        .map(Value::Int)
                        .map_err(|_| E::custom(format!("{s:?} is neither a decimal integer nor a name")))
                }
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut a: A) -> Result<Value, A::Error> {
                let mut l = Vec::new();
                while let Some(x) = a.next_element()? {
                    l.push(x);
                }
                Ok(Value::List(l))
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(self, mut a: A) -> Result<Value, A::Error> {
                let mut r = BTreeMap::new();
                while let Some((k, v)) = a.next_entry::<String, Value>()? {
                    r.insert(k, v);
                }
                Ok(Value::Record(r))
            }
        }
        d.deserialize_any(V)
    }
}

impl JsonSchema for Value {
    fn schema_name() -> Cow<'static, str> {
        "Value".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "description": "A raw value: true/false; an integer (an int, a fixed's underlying integer, a float's bit pattern), as a decimal string outside ±2^53; a variant name; a table's or list's elements; or a record's fields.",
            "anyOf": [
                {"type": "boolean"},
                {"type": "integer"},
                {"type": "string", "pattern": "^-?[0-9]+$"},
                {"type": "string", "pattern": "^[a-z][a-z0-9_]*$"},
                {"type": "array", "items": {"$ref": "#/$defs/Value"}},
                {"type": "object", "additionalProperties": {"$ref": "#/$defs/Value"}}
            ]
        })
    }
}

/// Named values: a state, inputs, params, a request or a response.
pub type Values = BTreeMap<String, Value>;

/// Field, variant and call names: `[a-z][a-z0-9_]*`.
pub fn is_name(s: &str) -> bool {
    let mut c = s.chars();
    c.next().is_some_and(|f| f.is_ascii_lowercase())
        && c.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Mechanic and interface ids: dot-separated segments of `[a-z0-9]+`
/// joined by single hyphens (`croc.walk`, `world.floor-height`).
pub fn is_id(s: &str) -> bool {
    !s.is_empty()
        && s.split('.').all(|seg| {
            !seg.is_empty()
                && seg
                    .split('-')
                    .all(|w| !w.is_empty() && w.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()))
        })
}

/// An interface reference: `<id>@<version>`.
pub fn parse_ref(s: &str) -> Option<(&str, u32)> {
    let (id, v) = s.rsplit_once('@')?;
    (is_id(id) && !v.is_empty() && v.chars().all(|c| c.is_ascii_digit())).then_some(())?;
    Some((id, v.parse().ok().filter(|v| *v >= 1)?))
}

/// Checks `values` against `fields`: every field present (unless `partial`
/// or listed in `skip`), nothing undeclared, each value of its type.
pub(crate) fn check_values(
    what: &str,
    values: &Values,
    fields: &BTreeMap<String, Type>,
    partial: bool,
    skip: &[String],
    problems: &mut Vec<String>,
) {
    for (name, v) in values {
        match fields.get(name) {
            None => problems.push(format!("{what}: {name} is not declared")),
            Some(t) => {
                if let Err(e) = t.check(v) {
                    problems.push(format!("{what}: {name}: {e}"));
                }
            }
        }
    }
    if !partial {
        for name in fields.keys().filter(|n| !values.contains_key(*n) && !skip.contains(n)) {
            problems.push(format!("{what}: {name} is missing"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(s: &str) -> Field {
        toml::from_str(s).unwrap()
    }

    #[test]
    fn types_check_their_keys() {
        assert_eq!(field("type = \"int\"\nbits = 16\nsigned = true").ty(), Ok(Type::Int { bits: 16, signed: true }));
        assert_eq!(field("type = \"int\"\nbits = 16").ty(), Err("int needs signed".into()));
        assert_eq!(field("type = \"bool\"\nbits = 1").ty(), Err("bits does not apply to bool".into()));
        assert_eq!(field("type = \"float\"\nbits = 16").ty(), Err("float bits 16: 32, 64 or 80".into()));
        assert!(field("type = \"fixed\"\nsigned = true\nint_bits = 0\nfrac_bits = 12").ty().is_err());
        assert!(toml::from_str::<Field>("type = \"fixed\"\nsigned = true\nint_bits = 4\nframe_bits = 12").is_err());
        let t =
            field("type = \"table\"\nlen = 2\nof = { type = \"fixed\", signed = false, int_bits = 4, frac_bits = 12 }");
        assert_eq!(
            t.ty().unwrap(),
            Type::Table { of: Box::new(Type::Fixed { signed: false, int_bits: 4, frac_bits: 12 }), len: 2 }
        );
        assert!(
            field("type = \"table\"\nlen = 2\nof = { type = \"table\", len = 1, of = { type = \"bool\" } }")
                .ty()
                .is_err()
        );
        assert!(field("type = \"enum\"\nvariants = [\"idle\", \"idle\"]").ty().is_err());
        let piece = field(
            "type = \"list\"\nof = { type = \"record\", fields = { x = { type = \"int\", bits = 32, signed = true }, \
             edges = { type = \"table\", len = 4, of = { type = \"record\", fields = { d = { type = \"int\", bits = 32, signed = true } } } } } }",
        )
        .ty()
        .unwrap();
        let s32 = Type::Int { bits: 32, signed: true };
        let edge = Type::Record([("d".to_string(), s32.clone())].into());
        assert_eq!(
            piece,
            Type::List {
                of: Box::new(Type::Record(
                    [("edges".to_string(), Type::Table { of: Box::new(edge), len: 4 }), ("x".to_string(), s32)].into()
                ))
            }
        );
        assert!(piece.is_data());
        assert_eq!(field("type = \"record\"\nfields = {}").ty(), Err("a record needs at least one field".into()));
        assert_eq!(
            field("type = \"list\"\nof = { type = \"bool\" }\nlen = 2").ty(),
            Err("len does not apply to list".into())
        );
    }

    #[test]
    fn values_fit_their_types() {
        let s16 = Type::Int { bits: 16, signed: true };
        assert!(s16.check(&Value::Int(-32768)).is_ok());
        assert!(s16.check(&Value::Int(32768)).is_err());
        assert!(Type::Fixed { signed: false, int_bits: 4, frac_bits: 12 }.check(&Value::Int(0xFFFF)).is_ok());
        assert!(Type::Float { bits: 80 }.check(&Value::Int((1 << 80) - 1)).is_ok());
        let e = Type::Enum(vec!["idle".into(), "walk".into()]);
        assert!(e.check(&Value::Variant("walk".into())).is_ok());
        assert!(e.check(&Value::Variant("run".into())).is_err());
        let t = Type::Table { of: Box::new(Type::Bool), len: 2 };
        assert_eq!(t.check(&Value::List(vec![Value::Bool(true)])), Err("expected 2 elements, got 1".into()));
        let r = Type::Record([("x".to_string(), s16.clone())].into());
        let l = Type::List { of: Box::new(r) };
        let rec = |x: i128| Value::Record([("x".to_string(), Value::Int(x))].into());
        assert!(l.check(&Value::List(vec![])).is_ok());
        assert!(l.check(&Value::List(vec![rec(1), rec(-2)])).is_ok());
        assert_eq!(
            l.check(&Value::List(vec![rec(1), rec(40000)])),
            Err("[1]: .x: 40000 is outside -32768..=32767".into())
        );
        assert_eq!(l.check(&Value::List(vec![Value::Record(BTreeMap::new())])), Err("[0]: x is missing".into()));
        let v: Value = serde_json::from_str(r#"[{"x": 1}]"#).unwrap();
        assert_eq!(v, Value::List(vec![rec(1)]));
        assert_eq!(serde_json::to_string(&v).unwrap(), r#"[{"x":1}]"#);
    }

    #[test]
    fn big_integers_are_strings_in_json() {
        let big = Value::Int(1 << 60);
        assert_eq!(serde_json::to_string(&big).unwrap(), "\"1152921504606846976\"");
        assert_eq!(serde_json::from_str::<Value>("\"1152921504606846976\"").unwrap(), big);
        assert_eq!(serde_json::from_str::<Value>("[1, true, \"walk\"]").unwrap(), {
            Value::List(vec![Value::Int(1), Value::Bool(true), Value::Variant("walk".into())])
        });
        assert!(serde_json::from_str::<Value>("1.5").is_err());
    }

    #[test]
    fn ids_and_refs() {
        assert!(
            is_id("croc.walk") && is_id("world.floor-height") && !is_id("Croc.walk") && !is_id("a..b") && !is_id("a-")
        );
        assert_eq!(parse_ref("world.floor-height@1"), Some(("world.floor-height", 1)));
        assert_eq!(parse_ref("world.floor-height"), None);
        assert_eq!(parse_ref("rng@0"), None);
    }
}
