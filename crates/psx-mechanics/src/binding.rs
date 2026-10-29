//! `mechanics/<id>/binding.toml`: where one release of the game keeps a
//! mechanic's fields, and how its step is bracketed.
//!
//! The shape is the same for every platform; locations, call sites and the
//! capture bracket are the platform's vocabulary ([`Platform`]).

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::interface::{CallKind, Interface};
use crate::mechanic::Mechanic;
use crate::value::{Type, Value, parse_ref};

/// A binding, generic over the platform's location (`L`), call site (`S`)
/// and capture bracket (`C`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, bound(deserialize = "L: Deserialize<'de>, S: Deserialize<'de>, C: Deserialize<'de>"))]
pub struct Binding<L, S, C> {
    pub mechanic: String,
    pub mechanic_version: u32,
    pub source: Source,
    /// Every state field: where it is, or why it cannot be read.
    #[serde(default)]
    pub state: BTreeMap<String, Encoding<L>>,
    /// Every input, as the step sees it.
    #[serde(default)]
    pub inputs: BTreeMap<String, Read<L>>,
    /// Every param: its value, or where capture reads it once, at the first
    /// step's start.
    #[serde(default)]
    pub params: BTreeMap<String, Param<L>>,
    /// Where each param's value comes from, in words.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params_provenance: BTreeMap<String, String>,
    /// `"<id>@<version>"` -> call -> how capture sees the call.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub interfaces: BTreeMap<String, BTreeMap<String, CallBinding<L, S>>>,
    pub capture: C,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Source {
    /// The producer's platform: `psx`.
    pub platform: String,
    /// The release, in words: `Croc (Europe) SLES-00593`.
    pub title: String,
    /// The image's identity as the producer reports it (for psx, the media
    /// identity of `psx context`).
    pub image_sha256: String,
}

impl Source {
    pub(crate) fn check(&self, platform: &str, p: &mut Vec<String>) {
        if self.platform != platform {
            p.push(format!("source.platform {:?}: this producer is {platform:?}", self.platform));
        }
        if self.image_sha256.len() != 64 || !self.image_sha256.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')) {
            p.push("source.image_sha256: 64 lowercase hex digits".into());
        }
    }
}

/// A state field's encoding, or why it cannot be read.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Encoding<L> {
    Read(Read<L>),
    Unobserved(Unobserved),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Unobserved {
    /// Always `true`: the field is not in memory the producer can read.
    /// Replay does not compare it.
    pub unobserved: bool,
    pub reason: String,
}

/// How a field's raw value is read: each part in order, first part most
/// significant, then `mask`, `shift`, sign extension, rescaling and `map`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Read<L> {
    pub parts: Vec<Part<L>>,
    /// The parts' byte order; the platform's when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endian: Option<Endian>,
    /// Applied to the concatenated parts, before `shift`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask: Option<Value>,
    /// Right shift after `mask`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shift: Option<u32>,
    /// Sign-extend from the read width (the mask's, or the parts').
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stored_signed: bool,
    /// `fixed`: the stored fraction bits, when they differ from the
    /// mechanic's. Capture refuses a rescale that loses bits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stored_frac_bits: Option<u32>,
    /// `enum` (required) and `bool`: stored integer (decimal) -> value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub map: Option<BTreeMap<String, Value>>,
    /// Table params: bytes from one element to the next; the parts read
    /// the first. Defaults to the parts' size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stride: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Part<L> {
    pub at: L,
    pub bytes: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Endian {
    Big,
    Little,
}

/// A param: its value, or where to read it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Param<L> {
    Value(Value),
    Read(Read<L>),
}

/// How capture sees one interface call: where it happens, and where the
/// request and response are read.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, bound(deserialize = "L: Deserialize<'de>, S: Deserialize<'de>"))]
pub struct CallBinding<L, S> {
    pub on: S,
    #[serde(default)]
    pub request: BTreeMap<String, Read<L>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub response: BTreeMap<String, Read<L>>,
}

/// Where a location is used, for the platform's checks.
pub enum Role<'a, S> {
    State,
    Input,
    Param,
    Request(&'a S),
    Response(&'a S),
    /// A glossary quantity, read whenever a tick ends.
    Quantity,
}

impl<S> Clone for Role<'_, S> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<S> Copy for Role<'_, S> {}

/// A producer's vocabulary for bindings.
pub trait Platform {
    type Location;
    type CallSite;
    type Capture;
    /// What ends one tick of the game's logic, for the glossary.
    type Tick;
    /// `source.platform`.
    const NAME: &'static str;
    /// The byte order parts are read in when `endian` is left out.
    const ENDIAN: Endian;
    fn check_location(at: &Self::Location, role: Role<'_, Self::CallSite>) -> Result<(), String>;
    fn check_call_site(on: &Self::CallSite) -> Result<(), String>;
    fn check_capture(capture: &Self::Capture, problems: &mut Vec<String>);
    fn check_tick(tick: &Self::Tick, problems: &mut Vec<String>);
}

/// The binding type of a platform.
pub type BindingFor<P> = Binding<<P as Platform>::Location, <P as Platform>::CallSite, <P as Platform>::Capture>;

impl<L, S, C> Binding<L, S, C> {
    /// What is wrong with the binding, for `mechanic` and its interfaces.
    pub fn check<P>(&self, mechanic: &Mechanic, interfaces: &[Interface]) -> Vec<String>
    where
        P: Platform<Location = L, CallSite = S, Capture = C>,
    {
        let mut p = Vec::new();
        if self.mechanic != mechanic.id || self.mechanic_version != mechanic.version {
            p.push(format!(
                "binds {}@{}, the mechanic is {}@{}",
                self.mechanic, self.mechanic_version, mechanic.id, mechanic.version
            ));
        }
        self.source.check(P::NAME, &mut p);
        let types = mechanic.types(&mut Vec::new());
        let unknown = |what: &str, names: Vec<&String>, declared: &BTreeMap<String, Type>, p: &mut Vec<String>| {
            for n in names.into_iter().filter(|n| !declared.contains_key(*n)) {
                p.push(format!("{what}.{n}: the mechanic has no such field"));
            }
        };
        unknown("state", self.state.keys().collect(), &types.state, &mut p);
        unknown("inputs", self.inputs.keys().collect(), &types.inputs, &mut p);
        unknown("params", self.params.keys().collect(), &types.params, &mut p);
        unknown("params_provenance", self.params_provenance.keys().collect(), &types.params, &mut p);
        for (what, declared, bound) in [
            ("state", &types.state, self.state.keys().collect::<Vec<_>>()),
            ("inputs", &types.inputs, self.inputs.keys().collect()),
            ("params", &types.params, self.params.keys().collect()),
        ] {
            for n in declared.keys().filter(|n| !bound.contains(n)) {
                p.push(format!("{what}.{n}: not bound"));
            }
        }
        for (n, e) in &self.state {
            let Some(ty) = types.state.get(n) else { continue };
            match e {
                Encoding::Read(r) => check_read::<P>(&format!("state.{n}"), r, ty, Role::State, &mut p),
                Encoding::Unobserved(u) => {
                    if !u.unobserved || u.reason.trim().is_empty() {
                        p.push(format!("state.{n}: unobserved = true with a reason, or parts"));
                    }
                }
            }
        }
        for (n, r) in &self.inputs {
            if let Some(ty) = types.inputs.get(n) {
                check_read::<P>(&format!("inputs.{n}"), r, ty, Role::Input, &mut p);
            }
        }
        for (n, v) in &self.params {
            let Some(ty) = types.params.get(n) else { continue };
            match v {
                Param::Value(v) => {
                    if let Err(e) = ty.check(v) {
                        p.push(format!("params.{n}: {e}"));
                    }
                }
                Param::Read(r) => check_read::<P>(&format!("params.{n}"), r, ty, Role::Param, &mut p),
            }
        }
        let required = mechanic.required(interfaces);
        for key in self.interfaces.keys() {
            if !mechanic.requires.contains(key) {
                p.push(format!("interfaces.{key:?}: the mechanic does not require it"));
            } else if parse_ref(key).is_none() {
                p.push(format!("interfaces.{key:?}: not <id>@<version>"));
            }
        }
        for i in required {
            let r = i.reference();
            let Some(calls) = self.interfaces.get(&r) else {
                p.push(format!("interfaces.{r:?}: not bound"));
                continue;
            };
            let call_types = i.types(&mut Vec::new());
            for name in calls.keys().filter(|c| !call_types.contains_key(*c)) {
                p.push(format!("interfaces.{r:?}.{name}: the interface has no such call"));
            }
            for (name, ct) in &call_types {
                let at = format!("interfaces.{r:?}.{name}");
                let Some(cb) = calls.get(name) else {
                    p.push(format!("{at}: not bound"));
                    continue;
                };
                if let Err(e) = P::check_call_site(&cb.on) {
                    p.push(format!("{at}.on: {e}"));
                }
                if ct.kind == CallKind::Command && !cb.response.is_empty() {
                    p.push(format!("{at}: a command has no response"));
                }
                for (side, declared, bound) in
                    [("request", &ct.request, &cb.request), ("response", &ct.response, &cb.response)]
                {
                    for n in bound.keys().filter(|n| !declared.contains_key(*n)) {
                        p.push(format!("{at}.{side}.{n}: the call has no such field"));
                    }
                    for (n, ty) in declared {
                        let role = if side == "request" { Role::Request(&cb.on) } else { Role::Response(&cb.on) };
                        match bound.get(n) {
                            None => p.push(format!("{at}.{side}.{n}: not bound")),
                            Some(r) => check_read::<P>(&format!("{at}.{side}.{n}"), r, ty, role, &mut p),
                        }
                    }
                }
            }
        }
        P::check_capture(&self.capture, &mut p);
        p
    }
}

/// Gives the bytes at a location plus an offset, in the order they are
/// stored: `fetch(at, offset, bytes)` (see [`Read::decode`]).
pub type Fetch<'f, L> = dyn FnMut(&L, u32, u32) -> Result<Vec<u8>, String> + 'f;

impl<L> Read<L> {
    /// The parts' size in bytes.
    pub fn bytes(&self) -> u32 {
        self.parts.iter().map(|p| p.bytes).sum()
    }

    /// The field's value, read as "Reading a field" in docs/mechanics.md
    /// says. `fetch(at, offset, bytes)` gives the bytes at a location plus
    /// `offset` in the order they are stored (`offset` is a table element's,
    /// 0 otherwise). An error is a refusal: a value that does not fit `ty`,
    /// a lossy rescale, or a stored integer the map does not name.
    pub fn decode(&self, ty: &Type, endian: Endian, fetch: &mut Fetch<'_, L>) -> Result<Value, String> {
        match ty {
            Type::Table { of, len } => {
                let stride = self.stride.unwrap_or(self.bytes());
                let items = (0..*len)
                    .map(|i| self.decode_at(of, endian, i * stride, fetch).map_err(|e| format!("[{i}]: {e}")))
                    .collect::<Result<_, _>>()?;
                Ok(Value::List(items))
            }
            t => self.decode_at(t, endian, 0, fetch),
        }
    }

    fn decode_at(&self, ty: &Type, endian: Endian, offset: u32, fetch: &mut Fetch<'_, L>) -> Result<Value, String> {
        let mut raw: u128 = 0;
        for p in &self.parts {
            let mut b = fetch(&p.at, offset, p.bytes)?;
            if b.len() != p.bytes as usize {
                return Err(format!("read {} bytes of {}", b.len(), p.bytes));
            }
            if self.endian.unwrap_or(endian) == Endian::Little {
                b.reverse();
            }
            for x in b {
                raw = raw << 8 | x as u128;
            }
        }
        let mut width = self.bytes() * 8;
        if let Some(Value::Int(m)) = self.mask {
            raw &= m as u128;
            width = 128 - (m as u128).leading_zeros();
        }
        let shift = self.shift.unwrap_or(0);
        raw >>= shift;
        width = width.saturating_sub(shift);
        let mut n = if self.stored_signed && width > 0 && width < 128 && raw >> (width - 1) & 1 == 1 {
            raw as i128 - (1i128 << width)
        } else {
            i128::try_from(raw).map_err(|_| format!("stored value {raw} does not fit"))?
        };
        if let (Type::Fixed { frac_bits, .. }, Some(stored)) = (ty, self.stored_frac_bits) {
            if stored > *frac_bits {
                let d = stored - frac_bits;
                if n & ((1i128 << d) - 1) != 0 {
                    return Err(format!(
                        "{n} with {stored} fraction bits loses bits as {frac_bits} (stored_frac_bits)"
                    ));
                }
                n >>= d;
            } else {
                n = n.checked_shl(frac_bits - stored).ok_or_else(|| format!("{n} does not fit"))?;
            }
        }
        let v = match (&self.map, ty) {
            (Some(map), _) => {
                map.get(&n.to_string()).cloned().ok_or_else(|| format!("stored value {n} is not in the map"))?
            }
            (None, Type::Bool) => Value::Bool(n != 0),
            (None, _) => Value::Int(n),
        };
        ty.check(&v).map_err(|e| format!("stored value {n}: {e}"))?;
        Ok(v)
    }
}

pub(crate) fn check_read<P: Platform>(
    at: &str,
    r: &Read<P::Location>,
    ty: &Type,
    role: Role<'_, P::CallSite>,
    p: &mut Vec<String>,
) {
    let table = matches!(role, Role::Param) && matches!(ty, Type::Table { .. });
    let element = match ty {
        Type::Table { of, .. } => of,
        t => t,
    };
    if r.parts.is_empty() {
        p.push(format!("{at}: parts: at least one"));
    }
    let bytes: u32 = r.parts.iter().map(|x| x.bytes).sum();
    if r.parts.iter().any(|x| x.bytes == 0) || bytes > 16 {
        p.push(format!("{at}: parts: 1 to 16 bytes in all"));
    }
    for (i, part) in r.parts.iter().enumerate() {
        if let Err(e) = P::check_location(&part.at, role) {
            p.push(format!("{at}: parts[{i}]: {e}"));
        }
    }
    let width = bytes * 8;
    if let Some(m) = &r.mask {
        if !matches!(m, Value::Int(m) if *m > 0 && (width >= 128 || *m < (1i128 << width))) {
            p.push(format!("{at}: mask must be a positive integer within the parts' {width} bits"));
        }
    }
    if r.shift.is_some_and(|s| s >= width) {
        p.push(format!("{at}: shift must be under the parts' {width} bits"));
    }
    if r.stride.is_some() && !table {
        p.push(format!("{at}: stride is for table params"));
    }
    if r.stride == Some(0) {
        p.push(format!("{at}: stride must be at least 1"));
    }
    if r.stored_frac_bits.is_some() && !matches!(element, Type::Fixed { .. }) {
        p.push(format!("{at}: stored_frac_bits is for fixed fields"));
    }
    if let Type::Float { bits } = element {
        if width != bits.next_multiple_of(8) || r.mask.is_some() || r.shift.is_some() || r.stored_signed {
            p.push(format!("{at}: a {bits}-bit float is read whole, {} bytes", bits.div_ceil(8)));
        }
    }
    match (element, &r.map) {
        (Type::Enum(_), None) => p.push(format!("{at}: an enum needs map (stored integer -> variant)")),
        (Type::Enum(_) | Type::Bool, Some(map)) => {
            for (k, v) in map {
                if k.parse::<i128>().is_err() {
                    p.push(format!("{at}: map key {k:?} is not a decimal integer"));
                }
                if let Err(e) = element.check(v) {
                    p.push(format!("{at}: map[{k}]: {e}"));
                }
            }
        }
        (_, Some(_)) => p.push(format!("{at}: map is for enum and bool fields")),
        _ => {}
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::interface::tests::FLOOR;
    use crate::mechanic::tests::WALK;

    /// A platform with plain addresses, for the generic checks.
    pub(crate) struct Flat;
    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
    pub(crate) struct Addr {
        address: u32,
    }
    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
    pub(crate) struct Entry {
        entry: u32,
    }
    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
    pub(crate) struct Bracket {
        begin: u32,
    }
    impl Platform for Flat {
        type Location = Addr;
        type CallSite = Entry;
        type Capture = Bracket;
        type Tick = Addr;
        const NAME: &'static str = "flat";
        const ENDIAN: Endian = Endian::Little;
        fn check_location(at: &Addr, _: Role<'_, Entry>) -> Result<(), String> {
            (at.address != 0).then_some(()).ok_or("address 0".into())
        }
        fn check_call_site(_: &Entry) -> Result<(), String> {
            Ok(())
        }
        fn check_capture(_: &Bracket, _: &mut Vec<String>) {}
        fn check_tick(_: &Addr, _: &mut Vec<String>) {}
    }

    const BINDING: &str = r#"
mechanic = "croc.walk"
mechanic_version = 1
source = { platform = "flat", title = "Croc", image_sha256 = "0000000000000000000000000000000000000000000000000000000000000000" }
capture = { begin = 1 }

[state.speed]
parts = [{ at = { address = 0x40 }, bytes = 4 }]
stored_signed = true

[state.mode]
parts = [{ at = { address = 0x44 }, bytes = 1 }]
map = { "0" = "idle", "1" = "walk" }

[inputs.forward]
parts = [{ at = { address = 0x50 }, bytes = 2 }]
mask = 0x1000

[params]
target = 35

[params.curve]
parts = [{ at = { address = 0x60 }, bytes = 1 }]
stride = 4

[params_provenance]
target = "immediate operand"

[interfaces."world.floor-height@1".height]
on = { entry = 0x100 }
request.x = { parts = [{ at = { address = 0x70 }, bytes = 4 }] }
request.z = { parts = [{ at = { address = 0x74 }, bytes = 4 }] }
response.y = { parts = [{ at = { address = 0x78 }, bytes = 4 }] }

[interfaces."world.floor-height@1".splash]
on = { entry = 0x200 }
request.x = { parts = [{ at = { address = 0x70 }, bytes = 4 }] }
"#;

    fn parse(s: &str) -> BindingFor<Flat> {
        toml::from_str(s).unwrap()
    }

    #[test]
    fn a_full_binding_checks() {
        let m = Mechanic::parse(WALK).unwrap();
        let i = [Interface::parse(FLOOR).unwrap()];
        let b = parse(BINDING);
        assert_eq!(b.check::<Flat>(&m, &i), Vec::<String>::new());
        assert_eq!(b.params["target"], Param::Value(Value::Int(35)));
        assert!(matches!(b.params["curve"], Param::Read(_)));
    }

    #[test]
    fn gaps_and_mistakes_are_named() {
        let m = Mechanic::parse(WALK).unwrap();
        let i = [Interface::parse(FLOOR).unwrap()];
        let mut b = parse(BINDING);
        b.state.remove("mode");
        b.params.insert("target".into(), Param::Value(Value::Int(70000)));
        b.interfaces.get_mut("world.floor-height@1").unwrap().remove("splash");
        let Encoding::Read(r) = b.state.get_mut("speed").unwrap() else { panic!() };
        r.stride = Some(4);
        r.parts[0].at.address = 0;
        assert_eq!(
            b.check::<Flat>(&m, &i),
            vec![
                "state.mode: not bound",
                "state.speed: parts[0]: address 0",
                "state.speed: stride is for table params",
                "params.target: 70000 is outside 0..=65535",
                "interfaces.\"world.floor-height@1\".splash: not bound",
            ]
        );
        let unobserved = BINDING.replace(
            "[state.mode]\nparts = [{ at = { address = 0x44 }, bytes = 1 }]\nmap = { \"0\" = \"idle\", \"1\" = \"walk\" }",
            "[state.mode]\nunobserved = true\nreason = \"kept in a register\"",
        );
        assert_eq!(parse(&unobserved).check::<Flat>(&m, &i), Vec::<String>::new());
    }

    /// Reads `mem` (byte i at address i).
    fn decode(r: &str, ty: &Type, mem: &[u8]) -> Result<Value, String> {
        let r: Read<Addr> = toml::from_str(r).unwrap();
        r.decode(ty, Endian::Little, &mut |at: &Addr, off, n| {
            let a = (at.address + off) as usize;
            mem.get(a..a + n as usize).map(<[u8]>::to_vec).ok_or("out of memory".into())
        })
    }

    #[test]
    fn fields_are_decoded_step_by_step() {
        let mem = [0x34, 0x12, 0xF0, 0xFF, 0x02, 0x00, 0x80, 0x01, 0x03, 0x00];
        let int = |bits, signed| Type::Int { bits, signed };
        // Little-endian by default; big when asked; parts join first-most-significant.
        assert_eq!(
            decode("parts = [{ at = { address = 0 }, bytes = 2 }]", &int(16, false), &mem),
            Ok(Value::Int(0x1234))
        );
        assert_eq!(
            decode("parts = [{ at = { address = 0 }, bytes = 2 }]\nendian = \"big\"", &int(16, false), &mem),
            Ok(Value::Int(0x3412))
        );
        assert_eq!(
            decode(
                "parts = [{ at = { address = 4 }, bytes = 1 }, { at = { address = 0 }, bytes = 1 }]",
                &int(16, false),
                &mem
            ),
            Ok(Value::Int(0x0234))
        );
        // Sign extension from the parts' width, or the mask's after the shift.
        assert_eq!(
            decode("parts = [{ at = { address = 2 }, bytes = 2 }]\nstored_signed = true", &int(16, true), &mem),
            Ok(Value::Int(-16))
        );
        assert_eq!(
            decode(
                "parts = [{ at = { address = 2 }, bytes = 2 }]\nmask = 0xF0\nshift = 4\nstored_signed = true",
                &int(4, true),
                &mem
            ),
            Ok(Value::Int(-1))
        );
        // A value that does not fit is refused.
        assert_eq!(
            decode("parts = [{ at = { address = 2 }, bytes = 2 }]", &int(8, false), &mem),
            Err("stored value 65520: 65520 is outside 0..=255".into())
        );
        // Rescales: up is exact, down refuses lost bits.
        let fx = Type::Fixed { signed: false, int_bits: 4, frac_bits: 4 };
        assert_eq!(
            decode("parts = [{ at = { address = 4 }, bytes = 1 }]\nstored_frac_bits = 1", &fx, &mem),
            Ok(Value::Int(16))
        );
        assert_eq!(
            decode("parts = [{ at = { address = 4 }, bytes = 1 }]\nstored_frac_bits = 5", &fx, &mem),
            Ok(Value::Int(1))
        );
        assert_eq!(
            decode("parts = [{ at = { address = 8 }, bytes = 1 }]\nstored_frac_bits = 5", &fx, &mem),
            Err("3 with 5 fraction bits loses bits as 4 (stored_frac_bits)".into())
        );
        // Maps name stored integers; bools are otherwise nonzero.
        let mode = Type::Enum(vec!["idle".into(), "walk".into()]);
        let map = "parts = [{ at = { address = 7 }, bytes = 1 }]\nmap = { \"0\" = \"idle\", \"1\" = \"walk\" }";
        assert_eq!(decode(map, &mode, &mem), Ok(Value::Variant("walk".into())));
        assert_eq!(decode(&map.replace("= 7", "= 8"), &mode, &mem), Err("stored value 3 is not in the map".into()));
        assert_eq!(decode("parts = [{ at = { address = 6 }, bytes = 1 }]", &Type::Bool, &mem), Ok(Value::Bool(true)));
        // Tables step by the stride, or the parts' size.
        let table = Type::Table { of: Box::new(int(8, false)), len: 3 };
        assert_eq!(
            decode("parts = [{ at = { address = 4 }, bytes = 1 }]\nstride = 2", &table, &mem),
            Ok(Value::List(vec![Value::Int(2), Value::Int(0x80), Value::Int(3)]))
        );
        assert_eq!(
            decode("parts = [{ at = { address = 8 }, bytes = 1 }]", &table, &mem),
            Err("[2]: out of memory".into())
        );
    }
}
