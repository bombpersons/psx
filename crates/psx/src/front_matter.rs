//! The front matter of a Markdown file: the lines between two `---` lines
//! at its top, read as a small part of YAML (scalars, flow lists and flow
//! maps, a `#` comment after a value).

use std::collections::BTreeMap;

/// One value of the front matter.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Fm {
    Str(String),
    List(Vec<String>),
    Map(Vec<(String, String)>),
}

impl Fm {
    pub(crate) fn str(&self) -> &str {
        match self {
            Fm::Str(s) => s,
            _ => "",
        }
    }
    pub(crate) fn list(&self) -> Vec<String> {
        match self {
            Fm::Str(s) if s.is_empty() => Vec::new(),
            Fm::Str(s) => vec![s.clone()],
            Fm::List(l) => l.clone(),
            Fm::Map(m) => m.iter().map(|(k, _)| k.clone()).collect(),
        }
    }
}

fn unquote(s: &str) -> String {
    let s = s.trim();
    let q = s.len() >= 2 && ((s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')));
    if q { s[1..s.len() - 1].to_string() } else { s.to_string() }
}

/// `line` without a comment: a `#` outside quotes that starts the line or
/// follows a space.
fn uncomment(line: &str) -> &str {
    let mut quote = None;
    let mut prev = ' ';
    for (i, c) in line.char_indices() {
        match (c, quote) {
            ('"' | '\'', None) => quote = Some(c),
            (c, Some(q)) if c == q => quote = None,
            ('#', None) if prev.is_whitespace() => return line[..i].trim_end(),
            _ => {}
        }
        prev = c;
    }
    line
}

/// Items of a flow list or map body, split on top-level commas.
fn items(body: &str) -> Vec<String> {
    let (mut out, mut cur, mut depth, mut quote) = (Vec::new(), String::new(), 0i32, None);
    for c in body.chars() {
        match (c, quote) {
            ('"' | '\'', None) => quote = Some(c),
            (c, Some(q)) if c == q => quote = None,
            ('(' | '[' | '{', None) => depth += 1,
            (')' | ']' | '}', None) => depth -= 1,
            (',', None) if depth == 0 => {
                out.push(std::mem::take(&mut cur));
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    out.push(cur);
    out.into_iter().map(|s| unquote(&s)).filter(|s| !s.is_empty()).collect()
}

/// The front matter: scalars, flow lists (`[a, b]`, over several lines)
/// and flow maps (`{a: 1, b: 2}`). None when the text has none, or it is
/// never closed.
pub(crate) fn front_matter(text: &str) -> Option<BTreeMap<String, Fm>> {
    let mut lines = text.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }
    let mut out = BTreeMap::new();
    let mut open: Option<(String, String)> = None;
    for line in lines {
        if line.trim() == "---" {
            return Some(out);
        }
        let line = uncomment(line);
        if let Some((key, mut body)) = open.take() {
            body.push(' ');
            body.push_str(line.trim());
            open = Some((key, body));
        } else {
            match line.split_once(':') {
                Some((k, v)) if !k.starts_with(' ') && !k.trim().is_empty() => {
                    open = Some((k.trim().to_string(), v.trim().to_string()));
                }
                _ => continue,
            }
        }
        let Some((key, body)) = open.take() else { continue };
        let closes = |a: char, b: char| body.starts_with(a) && !body.ends_with(b);
        if closes('[', ']') || closes('{', '}') {
            open = Some((key, body));
            continue;
        }
        let v = if let Some(b) = body.strip_prefix('[').and_then(|b| b.strip_suffix(']')) {
            Fm::List(items(b))
        } else if let Some(b) = body.strip_prefix('{').and_then(|b| b.strip_suffix('}')) {
            Fm::Map(
                items(b)
                    .iter()
                    .map(|i| match i.split_once(':') {
                        Some((k, v)) => (unquote(k), unquote(v)),
                        None => (i.clone(), String::new()),
                    })
                    .collect(),
            )
        } else {
            Fm::Str(unquote(&body))
        };
        out.insert(key, v);
    }
    None
}

/// The text after the front matter (all of it when there is none).
pub(crate) fn body(text: &str) -> &str {
    let mut rest = text;
    let Some((first, after)) = rest.split_once('\n') else { return text };
    if first.trim() != "---" {
        return text;
    }
    rest = after;
    while let Some((line, after)) = rest.split_once('\n') {
        if line.trim() == "---" {
            return after;
        }
        rest = after;
    }
    if rest.trim() == "---" { "" } else { text }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comments_after_values_are_dropped() {
        let fm = front_matter(
            "---\nid: F-0007\ndepends: [F-0002, F-0005]   # car setup data, track surfaces\ntitle: \"Lap #2\" # the title\nmechanics: [a,  # first\n  b]\n---\nThe body.\n",
        )
        .unwrap();
        assert_eq!(fm["depends"].list(), ["F-0002", "F-0005"]);
        assert_eq!(fm["title"].str(), "Lap #2");
        assert_eq!(fm["mechanics"].list(), ["a", "b"]);
    }

    #[test]
    fn the_body_follows_the_front_matter() {
        assert_eq!(body("---\nid: a\n---\nThe body.\n"), "The body.\n");
        assert_eq!(body("No front matter.\n"), "No front matter.\n");
        assert_eq!(body("---\nid: a\n"), "---\nid: a\n");
    }
}
