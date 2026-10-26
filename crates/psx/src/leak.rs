//! The leak checker shared by everything at the barrier (`barrier check`,
//! and publishing and the message check after it): text that would carry
//! knowledge of the original's code to the implementer. Addresses and hex
//! constants, code-shaped text (C-like statements, MIPS registers and
//! instructions, decompiler output), the reverse-engineering tools and the
//! executable's name, the original's symbol names (from psx's own Ghidra and
//! PsyQ analysis), pointer-valued columns in observations, and runs of eight
//! words shared with the investigator's notes that are not already published.

use std::collections::{BTreeSet, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::Error;
use crate::commands::{Ctx, Op, Outcome};

/// Words in a run that counts as copied.
pub const RUN: usize = 8;

/// The kinds of finding, with what each means.
pub const KINDS: [(&str, &str); 8] = [
    ("address", "a PlayStation address (main RAM, scratchpad or hardware registers)"),
    ("hex_constant", "a hex constant"),
    ("code", "code-shaped text: a C-like statement, MIPS, decompiler output or a code block"),
    ("tool", "a reverse-engineering tool or the executable's name"),
    ("symbol", "one of the original's symbol names, from psx's analysis"),
    ("pointer_column", "an observation column whose values are addresses"),
    ("copied", "a run of eight words shared with the investigator's notes"),
    ("name", "a name given to the checker as the original's"),
];

#[derive(Clone, Debug)]
pub struct Finding {
    pub kind: &'static str,
    pub line: usize,
    pub text: String,
}

/// What the text is: prose (specs, messages), data (JSON: checked line by
/// line, but sharing words with the notes' data is expected) or CSV
/// observations (the header and cells, and pointer-valued columns).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Form {
    Prose,
    Data,
    Csv,
}

#[derive(Default)]
pub struct Checker {
    /// The original's identifiers, matched as whole words.
    pub symbols: BTreeSet<String>,
    /// Extra names (the executable's, say), matched as whole words, any case.
    pub names: BTreeSet<String>,
    /// Hashes of the notes' eight-word runs, less the published ones.
    pub runs: HashSet<u64>,
}

const REGISTERS: [&str; 32] = [
    "zero", "at", "v0", "v1", "a0", "a1", "a2", "a3", "t0", "t1", "t2", "t3", "t4", "t5", "t6", "t7", "s0", "s1", "s2",
    "s3", "s4", "s5", "s6", "s7", "t8", "t9", "k0", "k1", "gp", "sp", "fp", "ra",
];

const MNEMONICS: [&str; 58] = [
    "lb", "lbu", "lh", "lhu", "lw", "lwl", "lwr", "sb", "sh", "sw", "swl", "swr", "lui", "addi", "addiu", "add",
    "addu", "sub", "subu", "slt", "sltu", "slti", "sltiu", "and", "andi", "or", "ori", "xor", "xori", "nor", "sll",
    "srl", "sra", "sllv", "srlv", "srav", "mult", "multu", "div", "divu", "mfhi", "mflo", "mthi", "mtlo", "j", "jal",
    "jr", "jalr", "beq", "bne", "blez", "bgtz", "bltz", "bgez", "mfc0", "mtc0", "lwc2", "swc2",
];

const TOOLS: [&str; 12] = [
    "ghidra",
    "disasm",
    "mednafen",
    "duckstation",
    "debugger",
    "breakpoint",
    "breakpoints",
    "watchpoint",
    "watchpoints",
    "psyq",
    "objdump",
    "decomp",
];

/// Disc serial prefixes (SLUS_005.67 and the like): the executable's name.
const SERIALS: [&str; 15] = [
    "SCUS", "SLUS", "SCES", "SLES", "SCPS", "SLPS", "SLPM", "SCPM", "SIPS", "SCED", "SLED", "PAPX", "SCAJ", "SLKA",
    "SCKA",
];

const GHIDRA_PREFIXES: [&str; 7] = ["FUN_", "DAT_", "LAB_", "PTR_", "SUB_", "UNK_", "caseD_"];

fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// The words of a line with their byte offsets.
fn words(line: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, c) in line.char_indices().chain(std::iter::once((line.len(), ' '))) {
        match (is_word(c), start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                out.push((s, &line[s..i]));
                start = None;
            }
            _ => {}
        }
    }
    out
}

fn hex_digits(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// A word that is a PlayStation address: 80xxxxxx or A0xxxxxx (main RAM),
/// 1F80xxxx (scratchpad and hardware), with or without 0x.
fn address(w: &str) -> bool {
    let h = w.strip_prefix("0x").or_else(|| w.strip_prefix("0X")).unwrap_or(w);
    if h.len() != 8 || !hex_digits(h) {
        return false;
    }
    let h = h.to_ascii_lowercase();
    ((h.starts_with("80") || h.starts_with("a0")) && h.as_bytes()[2] <= b'1') || h.starts_with("1f80")
}

/// A value read as an address (an observation cell), unsigned or as i32.
fn pointer_value(s: &str) -> bool {
    let v = match s.trim().parse::<i64>() {
        Ok(v) if v < 0 => (v as i32) as u32 as i64,
        Ok(v) => v,
        Err(_) => return false,
    };
    (0x8000_0000..0x8020_0000).contains(&v) || (0x1F80_0000..0x1F80_0400).contains(&v)
}

fn hash(words: &[String]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    words.hash(&mut h);
    h.finish()
}

/// Lowercase words of text for runs: letters and digits.
fn plain_words(text: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        for (_, w) in words(line) {
            for part in w.split('_').filter(|p| !p.is_empty()) {
                out.push((n + 1, part.to_ascii_lowercase()));
            }
        }
    }
    out
}

/// Every run of `RUN` words in text.
pub fn runs(text: &str) -> HashSet<u64> {
    let w: Vec<String> = plain_words(text).into_iter().map(|(_, w)| w).collect();
    w.windows(RUN).map(hash).collect()
}

/// Whether a symbol name is distinctive enough to match as a word: not a
/// plain lowercase word (open, read, main) and not a default name.
fn distinctive(name: &str) -> bool {
    name.len() >= 4
        && name.chars().all(is_word)
        && !GHIDRA_PREFIXES.iter().any(|p| name.starts_with(p))
        && (name.chars().any(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'))
        && !name.chars().all(|c| c.is_ascii_uppercase() || c == '_')
}

impl Checker {
    pub fn add_symbol(&mut self, name: &str) {
        if distinctive(name) {
            self.symbols.insert(name.to_string());
        }
    }

    /// The findings in one line of prose (or a CSV header).
    fn line(&self, n: usize, line: &str, out: &mut Vec<Finding>) {
        let mut push = |kind: &'static str, text: &str| {
            if !out.iter().any(|f| f.line == n && f.kind == kind && f.text == text) {
                out.push(Finding { kind, line: n, text: text.to_string() });
            }
        };
        let ws = words(line);
        let before = |at: usize| line[..at].chars().next_back();
        for (i, &(at, w)) in ws.iter().enumerate() {
            let lower = w.to_ascii_lowercase();
            if address(w) {
                push("address", w);
            } else if (lower.starts_with("0x") && hex_digits(&w[2..])) || (before(at) == Some('$') && hex_digits(w)) {
                if before(at) == Some('$') && REGISTERS.contains(&lower.as_str()) {
                    push("code", &format!("${w}"));
                } else {
                    push("hex_constant", w);
                }
            }
            if before(at) == Some('$') && REGISTERS.contains(&lower.as_str()) {
                push("code", &format!("${w}"));
            }
            if GHIDRA_PREFIXES.iter().any(|p| w.starts_with(p) && hex_digits(&w[p.len()..])) {
                push("code", w);
            }
            if lower.starts_with("undefined") && w["undefined".len()..].chars().all(|c| c.is_ascii_digit()) {
                push("code", w);
            }
            // A mnemonic followed by a register: lw $v0, 0x10($sp) or lw v0,0x10(sp).
            if MNEMONICS.contains(&lower.as_str()) {
                let rest = line[at + w.len()..].trim_start();
                let next = ws.get(i + 1).map(|(_, x)| x.to_ascii_lowercase());
                if rest.starts_with('$')
                    || next.is_some_and(|x| {
                        REGISTERS.contains(&x.as_str()) && rest[x.len()..].trim_start().starts_with(',')
                    })
                {
                    push("code", line.trim());
                }
            }
            if TOOLS.contains(&lower.as_str()) || lower.starts_with("disassembl") || lower.starts_with("decompil") {
                push("tool", w);
            }
            if let Some(p) = SERIALS.iter().find(|p| w.len() > 4 && w.starts_with(*p)) {
                let rest = w[p.len()..].trim_start_matches(['_', '-']);
                if rest.len() >= 3 && rest.chars().take(3).all(|c| c.is_ascii_digit()) {
                    push("tool", w);
                }
            }
            if self.symbols.contains(w) {
                push("symbol", w);
            }
            if self.names.iter().any(|x| x.eq_ignore_ascii_case(w)) {
                push("name", w);
            }
        }
        // Names with a dot (TOMB2.EXE) as written.
        for x in self.names.iter().filter(|x| x.contains('.')) {
            if line.to_ascii_lowercase().contains(&x.to_ascii_lowercase()) {
                push("name", x);
            }
        }
        if line.contains("IDA Pro") {
            push("tool", "IDA Pro");
        }
        let t = line.trim();
        // A code block in a programming language (a data block, yaml or
        // json, is checked line by line like any text).
        let lang = t.strip_prefix("```").or_else(|| t.strip_prefix("~~~")).map(|l| l.trim().to_ascii_lowercase());
        if lang.is_some_and(|l| {
            ["c", "cpp", "c++", "h", "asm", "mips", "s", "rust", "rs", "python", "py", "js", "pseudo", "pseudocode"]
                .contains(&l.as_str())
        }) {
            push("code", t);
        }
        let table = t.starts_with('|');
        let between = |op: &str| {
            line.match_indices(op).any(|(i, _)| {
                let l = line[..i].trim_end().chars().next_back();
                let r = line[i + op.len()..].trim_start().chars().next();
                l.is_some_and(|c| is_word(c) || c == ')' || c == ']')
                    && r.is_some_and(|c| is_word(c) || c == '(' || c == '*')
            })
        };
        for op in ["->", "+=", "-=", "|=", "&=", "^=", "<<=", ">>=", "<<", ">>", "++", "--"] {
            let tight = op == "->" || op == "++" || op == "--";
            let hit = if tight {
                line.match_indices(op).any(|(i, _)| {
                    let l = line[..i].chars().next_back();
                    let r = line[i + op.len()..].chars().next();
                    match op {
                        "->" => l.is_some_and(is_word) && r.is_some_and(is_word),
                        _ => l.is_some_and(is_word) && !r.is_some_and(is_word) && !r.is_some_and(|c| c == '-'),
                    }
                })
            } else {
                between(op)
            };
            if hit && !(op == "--" && t.starts_with("--")) {
                push("code", t);
            }
        }
        if !table && (between("&&") || between("||")) {
            push("code", t);
        }
        // A statement: ends in ; with an assignment (not a comparison) or a
        // call; a control keyword with its parenthesised condition and a
        // brace or semicolon; else with a brace; return ...;.
        let assigns = line.match_indices('=').any(|(i, _)| {
            let l = line[..i].chars().next_back();
            let r = line[i + 1..].chars().next();
            !l.is_some_and(|c| "=!<>+-*/|&^".contains(c)) && r != Some('=') && line[..i].trim_end().ends_with(is_word)
        });
        let calls =
            ws.iter().any(|&(at, w)| line[at + w.len()..].starts_with('(') && w.chars().any(|c| c.is_alphabetic()));
        if t.ends_with(';') && (assigns || calls) {
            push("code", t);
        }
        let control = ["if", "while", "for", "switch"]
            .iter()
            .any(|k| ws.iter().any(|&(at, w)| w == *k && line[at + w.len()..].trim_start().starts_with('(')));
        if (control && (t.ends_with('{') || t.ends_with(';'))) || t.contains("else {") || t.starts_with("} else") {
            push("code", t);
        }
        if t.starts_with("return ") && t.ends_with(';') {
            push("code", t);
        }
    }

    /// The findings in a text.
    pub fn check(&self, text: &str, form: Form) -> Vec<Finding> {
        let mut out = Vec::new();
        let mut lines = text.lines();
        if form == Form::Csv {
            let header: Vec<&str> = lines.next().unwrap_or("").split(',').collect();
            self.line(1, &header.join(", "), &mut out);
            let mut pointers: Vec<Option<usize>> = vec![None; header.len()];
            for (n, row) in lines.enumerate() {
                for (i, cell) in row.split(',').enumerate() {
                    if address(cell.trim()) {
                        out.push(Finding { kind: "address", line: n + 2, text: cell.trim().to_string() });
                    }
                    match pointers.get_mut(i) {
                        Some(p) if p.is_none() && pointer_value(cell) => *p = Some(n + 2),
                        _ => {}
                    }
                }
            }
            for (i, p) in pointers.iter().enumerate() {
                if let Some(line) = p {
                    out.push(Finding { kind: "pointer_column", line: *line, text: header[i].to_string() });
                }
            }
        } else {
            for (n, line) in lines.enumerate() {
                self.line(n + 1, line, &mut out);
            }
        }
        if form == Form::Prose && !self.runs.is_empty() {
            let w = plain_words(text);
            let hit = |i: usize| {
                let key: Vec<String> = w[i..i + RUN].iter().map(|(_, x)| x.clone()).collect();
                self.runs.contains(&hash(&key))
            };
            let mut i = 0;
            while i + RUN <= w.len() {
                if !hit(i) {
                    i += 1;
                    continue;
                }
                // The whole shared run, however long.
                let mut end = i + RUN;
                while end < w.len() && hit(end + 1 - RUN) {
                    end += 1;
                }
                let text: Vec<&str> = w[i..end].iter().map(|(_, x)| x.as_str()).collect();
                out.push(Finding { kind: "copied", line: w[i].0, text: text.join(" ") });
                i = end;
            }
        }
        out.sort_by_key(|f| f.line);
        out
    }
}

/// Text files under a path (a file, or a directory walked without its hidden
/// directories), with the form each is checked in.
pub fn text_files(path: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut todo = vec![path.to_path_buf()];
    while let Some(p) = todo.pop() {
        if p.is_dir() {
            if let Ok(rd) = std::fs::read_dir(&p) {
                for e in rd.flatten() {
                    let name = e.file_name();
                    if !name.to_string_lossy().starts_with('.') {
                        todo.push(e.path());
                    }
                }
            }
        } else if p.is_file() {
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
            if p == path
                || ["md", "txt", "json", "jsonl", "csv", "toml", "py", "rs", "c", "h", "s"].contains(&ext.as_str())
            {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

pub fn form_of(p: &Path) -> Form {
    match p.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
        Some("csv") => Form::Csv,
        Some("json" | "jsonl" | "toml") => Form::Data,
        _ => Form::Prose,
    }
}

/// Files over this are skipped as notes (run outputs, traces).
pub(crate) const NOTES_LIMIT: u64 = 4 << 20;

pub fn read_text(p: &Path, limit: u64) -> Option<String> {
    let meta = std::fs::metadata(p).ok()?;
    if meta.len() > limit {
        return None;
    }
    let bytes = std::fs::read(p).ok()?;
    if bytes.iter().take(4096).any(|&b| b == 0) {
        return None;
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Every name in a `.symbols.json` (any `name` string in it) or `.sym` (the
/// last word of each line).
fn symbol_file_names(p: &Path) -> crate::Result<Vec<String>> {
    let text = std::fs::read_to_string(p).map_err(|e| Error::io(p, e))?;
    if p.to_string_lossy().ends_with(".json") {
        let v: Value = serde_json::from_str(&text).map_err(|e| Error::malformed(format!("{}: {e}", p.display())))?;
        let mut out = Vec::new();
        let mut todo = vec![&v];
        while let Some(v) = todo.pop() {
            match v {
                Value::Object(m) => {
                    if let Some(Value::String(s)) = m.get("name") {
                        out.push(s.clone());
                    }
                    todo.extend(m.values());
                }
                Value::Array(a) => todo.extend(a),
                _ => {}
            }
        }
        Ok(out)
    } else {
        Ok(text.lines().filter_map(|l| l.split_whitespace().last().map(str::to_string)).collect())
    }
}

/// The project's own analysis: Ghidra function names not Ghidra's defaults
/// (the investigator's renames included), and `psx symbols` output.
pub(crate) fn project_symbols(root: &Path) -> (Vec<String>, Vec<PathBuf>) {
    let (mut names, mut files) = (Vec::new(), Vec::new());
    let ghidra = root.join(".psx/ghidra");
    for key in std::fs::read_dir(&ghidra).into_iter().flatten().flatten() {
        for rev in std::fs::read_dir(key.path()).into_iter().flatten().flatten() {
            let index = rev.path().join("index.json");
            let Some(v) = std::fs::read(&index).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()) else {
                continue;
            };
            files.push(index);
            for f in v["functions"].as_array().into_iter().flatten() {
                match f["name"].as_str() {
                    Some(n) if f["name_source"] != "DEFAULT" => names.push(n.to_string()),
                    _ => {}
                }
            }
        }
    }
    for p in text_files(&root.join(".psx/symbols")) {
        if !p.to_string_lossy().ends_with(".symbols.json") {
            continue;
        }
        if let Ok(n) = symbol_file_names(&p) {
            names.extend(n);
            files.push(p);
        }
    }
    (names, files)
}

#[derive(clap::Args, Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct BarrierCheckArgs {
    /// Files or directories to check; .csv as observations.
    #[serde(default)]
    pub paths: Vec<PathBuf>,
    /// Text to check.
    #[arg(long)]
    pub text: Option<String>,
    /// The investigator's notes (text shared with them is refused); repeatable.
    #[arg(long)]
    #[serde(default)]
    pub notes: Vec<PathBuf>,
    /// Published text, which may be repeated; repeatable.
    #[arg(long)]
    #[serde(default)]
    pub published: Vec<PathBuf>,
    /// More .symbols.json or .sym files; repeatable.
    #[arg(long)]
    #[serde(default)]
    pub symbols: Vec<PathBuf>,
    /// A name to refuse (the executable's); repeatable.
    #[arg(long)]
    #[serde(default)]
    pub name: Vec<String>,
    /// A kind of finding to allow; repeatable.
    #[arg(long)]
    #[serde(default)]
    pub allow: Vec<String>,
}

impl BarrierCheckArgs {
    /// The checker these arguments describe, with what it was built from.
    pub fn checker(&self, ctx: &Ctx) -> crate::Result<(Checker, Value)> {
        let mut c = Checker::default();
        let (names, mut files) = project_symbols(&ctx.root);
        for n in &names {
            c.add_symbol(n);
        }
        for p in &self.symbols {
            let p = ctx.root.join(p);
            for n in symbol_file_names(&p)? {
                c.add_symbol(&n);
            }
            files.push(p);
        }
        c.names = self.name.iter().cloned().collect();
        let mut notes_files = 0;
        for p in &self.notes {
            for f in text_files(&ctx.root.join(p)) {
                if let Some(t) = read_text(&f, NOTES_LIMIT) {
                    c.runs.extend(runs(&t));
                    notes_files += 1;
                }
            }
        }
        for p in &self.published {
            for f in text_files(&ctx.root.join(p)) {
                if let Some(t) = read_text(&f, NOTES_LIMIT) {
                    for r in runs(&t) {
                        c.runs.remove(&r);
                    }
                }
            }
        }
        let sources = json!({
            "symbols": c.symbols.len(), "symbol_files": files,
            "notes_files": notes_files, "notes_runs": c.runs.len(),
        });
        Ok((c, sources))
    }
}

impl Op for BarrierCheckArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        Outcome::report((|| {
            for k in &self.allow {
                if !KINDS.iter().any(|(n, _)| n == k) {
                    let all: Vec<&str> = KINDS.iter().map(|(n, _)| *n).collect();
                    return Err(Error::invalid(format!("allow {k:?}: one of {}", all.join(", "))));
                }
            }
            if self.paths.is_empty() && self.text.is_none() {
                return Err(Error::invalid("nothing to check: give paths or text"));
            }
            let (c, sources) = self.checker(ctx)?;
            let mut findings = Vec::new();
            let mut allowed = 0usize;
            let mut checked = 0usize;
            let mut take = |file: Value, fs: Vec<Finding>| {
                for f in fs {
                    if self.allow.iter().any(|a| a == f.kind) {
                        allowed += 1;
                    } else {
                        findings.push(json!({"file": file, "line": f.line, "kind": f.kind, "text": f.text}));
                    }
                }
            };
            if let Some(t) = &self.text {
                take(json!(null), c.check(t, Form::Prose));
                checked += 1;
            }
            for p in &self.paths {
                let p = ctx.root.join(p);
                if !p.exists() {
                    return Err(Error::io(&p, std::io::Error::from(std::io::ErrorKind::NotFound)));
                }
                for f in text_files(&p) {
                    let Some(t) = read_text(&f, u64::MAX) else { continue };
                    take(json!(f), c.check(&t, form_of(&f)));
                    checked += 1;
                }
            }
            let clean = findings.is_empty();
            let mut kinds = serde_json::Map::new();
            for f in &findings {
                let k = f["kind"].as_str().unwrap_or_default();
                let meaning = KINDS.iter().find(|(n, _)| *n == k).map(|(_, m)| *m).unwrap_or_default();
                kinds.insert(k.to_string(), json!(meaning));
            }
            Ok((
                clean,
                json!({
                    "status": if clean { "clean" } else { "leaks" },
                    "checked": checked, "findings": findings.len(), "allowed": allowed,
                    "kinds": kinds, "results": findings, "sources": sources,
                    "next": if clean { "nothing found; the reviewer still reads it for what no check sees" }
                        else { "rewrite each finding in game terms (what the player sees, values as observed), then check again" },
                }),
            ))
        })())
    }
}

/// `barrier hook`: the check on each message the person sends the
/// implementer, run by the implementer's host before the agent sees it
/// (Claude Code's UserPromptSubmit). The event comes on stdin; the settings
/// in the implementer's `.claude/barrier.json`. Every message is logged to
/// `notes/guidance.jsonl` as guidance, a crossing or stopped, each entry with
/// the sha256 of the line before; a stopped message is logged with its
/// reasons only.
#[derive(clap::Args, Clone, Debug)]
pub struct BarrierHookArgs {
    /// The settings (default .claude/barrier.json in the project).
    #[arg(long)]
    pub config: Option<PathBuf>,
}

/// What the hook prints and its exit status: 0 lets the message through
/// (stdout is added to the agent's context), 2 stops it (stderr is shown to
/// the person).
pub struct HookOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit: u8,
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect()
}

impl BarrierHookArgs {
    pub fn run(&self, stdin: &str) -> HookOutput {
        match self.check(stdin) {
            Ok(o) => o,
            // Closed on failure: a message the check could not look at does not go through.
            Err(e) => HookOutput {
                stdout: String::new(),
                stderr: format!(
                    "Not sent: the barrier check could not run ({}). Tell whoever set up the port.\n",
                    e.to_value()["error"]["message"].as_str().unwrap_or("unknown error")
                ),
                exit: 2,
            },
        }
    }

    fn check(&self, stdin: &str) -> crate::Result<HookOutput> {
        let event: Value =
            serde_json::from_str(stdin).map_err(|e| Error::malformed(format!("the hook's event: {e}")))?;
        let prompt = event["prompt"].as_str().unwrap_or_default();
        let base = std::env::var_os("CLAUDE_PROJECT_DIR")
            .map(PathBuf::from)
            .or_else(|| event["cwd"].as_str().map(PathBuf::from))
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default();
        let at = |p: &str| if Path::new(p).is_absolute() { PathBuf::from(p) } else { base.join(p) };
        let config_path = match &self.config {
            Some(p) if p.is_absolute() => p.clone(),
            Some(p) => base.join(p),
            None => base.join(".claude/barrier.json"),
        };
        let config: Value =
            serde_json::from_slice(&std::fs::read(&config_path).map_err(|e| Error::io(&config_path, e))?)
                .map_err(|e| Error::malformed(format!("{}: {e}", config_path.display())))?;
        // dirty_paths and published_paths are the names the first hook used.
        let key = |a: &str, b: &str| if config.get(a).is_some() { config[a].clone() } else { config[b].clone() };
        let investigator = config["investigator"].as_str().map(at);
        let mut notes: Vec<PathBuf> = strings(&key("notes", "dirty_paths")).iter().map(|p| at(p)).collect();
        if notes.is_empty() {
            notes.extend(investigator.as_ref().map(|i| i.join("notes")));
        }
        let published = match key("published", "published_paths") {
            Value::Null => vec![base.join("specs"), base.join("questions")],
            v => strings(&v).iter().map(|p| at(p)).collect(),
        };
        let allow = strings(&config["allow"]);
        let mut c = Checker::default();
        if let Some(i) = &investigator {
            for n in project_symbols(i).0 {
                c.add_symbol(&n);
            }
        }
        for p in strings(&config["symbols"]) {
            for n in symbol_file_names(&at(&p))? {
                c.add_symbol(&n);
            }
        }
        c.names = strings(&config["names"]).into_iter().collect();
        if prompt.split_whitespace().nth(RUN - 1).is_some() {
            for p in &notes {
                for f in text_files(p) {
                    if let Some(t) = read_text(&f, NOTES_LIMIT) {
                        c.runs.extend(runs(&t));
                    }
                }
            }
            for p in &published {
                for f in text_files(p) {
                    if let Some(t) = read_text(&f, NOTES_LIMIT) {
                        for r in runs(&t) {
                            c.runs.remove(&r);
                        }
                    }
                }
            }
        }
        let prefix = config["confirm_prefix"].as_str().unwrap_or("[cross]");
        let confirmed = prompt.trim_start().starts_with(prefix);
        let mut reasons: Vec<&str> = Vec::new();
        for f in c.check(prompt, Form::Prose) {
            if !allow.iter().any(|a| a == f.kind) && !reasons.contains(&f.kind) {
                reasons.push(f.kind);
            }
        }
        let verdict = match (reasons.is_empty(), confirmed) {
            (true, _) => "guidance",
            (false, true) => "crossing",
            (false, false) => "stopped",
        };
        // The log, chained: each entry carries the sha256 of the line before.
        let log = at(config["log"].as_str().unwrap_or("notes/guidance.jsonl"));
        let mut text = std::fs::read_to_string(&log).unwrap_or_default();
        let prev = text.lines().rev().find(|l| !l.trim().is_empty()).map(|l| crate::publish::sha256(l.as_bytes()));
        let entry = json!({
            "time": crate::publish::utc_now(), "session": event["session_id"], "verdict": verdict,
            "reasons": reasons,
            // A stopped message never reached the agent; its text stays out of the repository.
            "prompt": if verdict == "stopped" { Value::Null } else { json!(prompt) },
            "prev": prev,
        });
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text += &entry.to_string();
        text.push('\n');
        if let Some(d) = log.parent() {
            std::fs::create_dir_all(d).map_err(|e| Error::io(d, e))?;
        }
        std::fs::write(&log, text).map_err(|e| Error::io(&log, e))?;
        let said: Vec<&str> =
            reasons.iter().map(|k| KINDS.iter().find(|(n, _)| n == k).map(|(_, m)| *m).unwrap_or(k)).collect();
        Ok(match verdict {
            "stopped" => HookOutput {
                stdout: String::new(),
                stderr: format!(
                    "Not sent: this looks like it carries knowledge of the original's code ({}). Describe what you \
                     see in the game instead, or tell the investigator so it becomes a spec. To send it anyway, \
                     start the message with {prefix}; it will be logged as a crossing.\n",
                    said.join("; ")
                ),
                exit: 2,
            },
            "crossing" => HookOutput {
                stdout: format!(
                    "The person sent this message with {prefix}, past the barrier check ({}). It may carry \
                     knowledge of the original's code. Before acting on it, write it up as a question in \
                     questions/ for the investigator, and act on the answer.\n",
                    said.join("; ")
                ),
                stderr: String::new(),
                exit: 0,
            },
            _ => HookOutput { stdout: String::new(), stderr: String::new(), exit: 0 },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{BarrierHookArgs, Checker, Form, runs};

    #[test]
    fn hook_stops_logs_and_crosses() {
        let dir = std::env::temp_dir().join(format!("psx-hook-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("notes-dirty")).unwrap();
        std::fs::create_dir_all(dir.join(".claude")).unwrap();
        let noted = "when she lands from a jump the camera dips a little before it settles";
        std::fs::write(dir.join("notes-dirty/a.md"), noted).unwrap();
        std::fs::write(dir.join(".claude/barrier.json"), r#"{"notes": ["notes-dirty"], "names": ["TOMB2.EXE"]}"#)
            .unwrap();
        let send = |prompt: &str| {
            let event = serde_json::json!({"prompt": prompt, "session_id": "t", "cwd": dir});
            BarrierHookArgs { config: Some(dir.join(".claude/barrier.json")) }.run(&event.to_string())
        };
        assert_eq!(send("she turns too fast").exit, 0);
        let stopped = send("watch 0x8017C87C in TOMB2.EXE");
        assert_eq!(stopped.exit, 2);
        assert!(stopped.stderr.contains("[cross]"));
        assert_eq!(send(noted).exit, 2);
        let crossed = send(&format!("[cross] {noted}"));
        assert_eq!(crossed.exit, 0);
        assert!(crossed.stdout.contains("question"));
        let log = std::fs::read_to_string(dir.join("notes/guidance.jsonl")).unwrap();
        let lines: Vec<serde_json::Value> = log.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        let verdicts: Vec<&str> = lines.iter().map(|l| l["verdict"].as_str().unwrap()).collect();
        assert_eq!(verdicts, ["guidance", "stopped", "stopped", "crossing"]);
        // A stopped message is logged by its reasons only.
        assert_eq!(lines[1]["prompt"], serde_json::Value::Null);
        assert_eq!(lines[1]["reasons"], serde_json::json!(["address", "name"]));
        assert_eq!(lines[0]["prev"], serde_json::Value::Null);
        let first = log.lines().next().unwrap();
        assert_eq!(lines[1]["prev"].as_str().unwrap(), crate::publish::sha256(first.as_bytes()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn kinds(c: &Checker, text: &str, form: Form) -> Vec<(&'static str, String)> {
        c.check(text, form).into_iter().map(|f| (f.kind, f.text)).collect()
    }

    #[test]
    fn addresses_hex_and_numbers() {
        let c = Checker::default();
        let k = |t: &str| kinds(&c, t, Form::Prose);
        assert_eq!(k("Lara is at 0x8017C87C."), vec![("address", "0x8017C87C".into())]);
        assert_eq!(k("the pad bits live at 8008aad4"), vec![("address", "8008aad4".into())]);
        assert_eq!(k("the GPU status 1F801814"), vec![("address", "1F801814".into())]);
        assert_eq!(k("flags 0x4000 mean active"), vec![("hex_constant", "0x4000".into())]);
        // Decimal numbers and plain words are not.
        assert!(k("she has 1000 health and moves 801234 units").is_empty());
        assert!(k("rows 1 to 4 run 2, 0, 0, 1 ticks").is_empty());
    }

    #[test]
    fn code_shaped_text() {
        let c = Checker::default();
        let code = |t: &str| kinds(&c, t, Form::Prose).iter().any(|(k, _)| *k == "code");
        assert!(code("lw $v0, 16($sp)"));
        assert!(code("addiu sp, sp, -24"));
        assert!(code("accumulator += fields"));
        assert!(code("speed = speed * 3 >> 2"));
        assert!(code("if (state == 2) {"));
        assert!(code("lara->speed = 0;"));
        assert!(code("undefined4 FUN_80011f2c(void)"));
        assert!(code("```c") && !code("```yaml") && !code("```"));
        assert!(code("count++"));
        assert!(code("return x;"));
        // Prose, Markdown tables and lists are not.
        assert!(!code("If she lands (on flat ground), she stops."));
        assert!(!code("| state | goal || speed |"));
        assert!(!code("- jumps back: 2 ticks; then lands."));
        assert!(!code("Speed rises by 3 per tick while forward is held."));
        assert!(!code("the hop -- one tick -- ends"));
        assert!(!code("For a back jump, she leans."));
    }

    #[test]
    fn tools_names_and_symbols() {
        let mut c = Checker::default();
        for s in ["LaraAboveWater", "CdRead", "lara_as_back", "main", "open", "FUN_80011f2c", "VSYNC"] {
            c.add_symbol(s);
        }
        c.names.insert("TOMB2.EXE".into());
        let k = |t: &str| kinds(&c, t, Form::Prose);
        assert_eq!(k("as Ghidra shows"), vec![("tool", "Ghidra".into())]);
        assert_eq!(k("the decompiled routine"), vec![("tool", "decompiled".into())]);
        assert_eq!(k("SLUS_004.37 boots"), vec![("tool", "SLUS_004".into())]);
        assert_eq!(k("CdRead is called"), vec![("symbol", "CdRead".into())]);
        assert_eq!(k("in lara_as_back"), vec![("symbol", "lara_as_back".into())]);
        // Plain words are not symbols, even when the original has them.
        assert!(k("open the main door").is_empty());
        assert_eq!(k("tomb2.exe loads"), vec![("name", "TOMB2.EXE".into())]);
    }

    #[test]
    fn observations_check_the_header_cells_and_pointers() {
        let c = Checker::default();
        let csv = "run_frame,frame,tick,x,next\n0,1,0,5,-2145907584\n1,2,1,6,2149059712\n";
        assert_eq!(kinds(&c, csv, Form::Csv), vec![("pointer_column", "next".into())]);
        let k = kinds(&c, "run_frame,0x8008AAD4\n0,0x80010000\n", Form::Csv);
        assert_eq!(k, vec![("address", "0x8008AAD4".into()), ("address", "0x80010000".into())]);
        assert!(kinds(&c, "run_frame,x\n0,1000\n", Form::Csv).is_empty());
    }

    #[test]
    fn runs_copied_from_notes_but_not_published() {
        let notes = "The control phase adds the fields elapsed to an accumulator and runs one tick per two fields.";
        let published = "She runs one tick per two fields of play, as the cases show.";
        let mut c = Checker { runs: runs(notes), ..Default::default() };
        for r in runs(published) {
            c.runs.remove(&r);
        }
        let k = kinds(&c, "Each field, the control phase adds the fields elapsed to an accumulator.", Form::Prose);
        assert_eq!(k, vec![("copied", "the control phase adds the fields elapsed to an accumulator".into())]);
        assert!(kinds(&c, "Lara runs one tick per two fields of play.", Form::Prose).is_empty());
    }
}
