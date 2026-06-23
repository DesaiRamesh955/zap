//! Structure-aware views over a file for token-lean research.
//!
//! Two modes:
//! - [`overview`] — a cheap, explicitly *lossy* map of a file (imports + signatures
//!   for code, heading outline for markdown, head/tail for text) with original line
//!   numbers so the agent can drill in.
//! - [`precise_lines`] / [`precise_symbol`] — *lossless* byte-exact slices for editing.
//!
//! Overview is for navigation and MUST never feed an edit; precise slices are exact.

use crate::core::filter::{is_import_line, is_signature_line, Language};
use crate::core::tracking::estimate_tokens;
use anyhow::{bail, Result};

/// Estimated-token size at or below which a file is shown in full rather than as an
/// overview. Skeletonizing a small file saves nothing and risks hiding content.
pub const OVERVIEW_TOKEN_THRESHOLD: usize = 400;

/// True if `content` is large enough that an overview is the better default.
pub fn should_overview(content: &str) -> bool {
    estimate_tokens(content) > OVERVIEW_TOKEN_THRESHOLD
}

enum ViewKind {
    Code,
    Markdown,
    Text,
}

fn classify(lang: &Language, path: &str) -> ViewKind {
    let ext = path.rsplit('.').next().unwrap_or("").to_lowercase();
    if matches!(ext.as_str(), "md" | "markdown" | "mdx") {
        return ViewKind::Markdown;
    }
    match lang {
        Language::Unknown | Language::Data => ViewKind::Text,
        _ => ViewKind::Code,
    }
}

/// Produce a lossy navigational overview of `content`, chosen by content type.
pub fn overview(content: &str, lang: &Language, path: &str) -> String {
    match classify(lang, path) {
        ViewKind::Code => overview_code(content, path),
        ViewKind::Markdown => overview_markdown(content, path),
        ViewKind::Text => overview_text(content, path),
    }
}

fn overview_code(content: &str, path: &str) -> String {
    let total = content.lines().count();
    let mut landmarks: Vec<(usize, String)> = Vec::new();
    for (i, line) in content.lines().enumerate() {
        let t = line.trim();
        let kept = is_import_line(t)
            || is_signature_line(t)
            || t.starts_with("const ")
            || t.starts_with("static ")
            || t.starts_with("pub const ")
            || t.starts_with("pub static ");
        if kept {
            landmarks.push((i + 1, t.to_string()));
        }
    }
    let mut out = format!(
        "{path}: overview — {} symbols, {total} lines. \
         Drill in: zap read {path} --symbol <name> | --lines A-B\n\n",
        landmarks.len()
    );
    for (n, text) in &landmarks {
        out.push_str(&format!("{n:>6}  {text}\n"));
    }
    out
}

fn overview_markdown(content: &str, path: &str) -> String {
    let total = content.lines().count();
    let mut headings: Vec<(usize, String)> = Vec::new();
    for (i, line) in content.lines().enumerate() {
        let t = line.trim_start();
        if t.starts_with('#') {
            headings.push((i + 1, t.to_string()));
        }
    }
    let mut out = format!(
        "{path}: outline — {} headings, {total} lines. \
         Drill in: zap read {path} --lines A-B\n\n",
        headings.len()
    );
    for (n, text) in &headings {
        out.push_str(&format!("{n:>6}  {text}\n"));
    }
    out
}

fn overview_text(content: &str, path: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len();
    let head = 20.min(total);
    let tail = if total > head + 10 { 10 } else { 0 };
    let mut out =
        format!("{path}: text preview — {total} lines. Drill in: zap read {path} --lines A-B\n\n");
    for (i, l) in lines.iter().take(head).enumerate() {
        out.push_str(&format!("{:>6}  {l}\n", i + 1));
    }
    if tail > 0 {
        out.push_str(&format!(
            "        [{} lines omitted]\n",
            total - head - tail
        ));
        for (i, line) in lines.iter().enumerate().skip(total - tail) {
            out.push_str(&format!("{:>6}  {}\n", i + 1, line));
        }
    } else if total > head {
        out.push_str(&format!("        [{} lines omitted]\n", total - head));
    }
    out
}

/// Byte-exact lines `start..=end` (1-based, inclusive) with original line numbers.
pub fn precise_lines(content: &str, start: usize, end: usize) -> Result<String> {
    let lines: Vec<&str> = content.lines().collect();
    if start < 1 || end < start || end > lines.len() {
        bail!(
            "invalid line range {}-{} for file with {} lines",
            start,
            end,
            lines.len()
        );
    }
    let width = end.to_string().len();
    let mut out = String::new();
    for n in start..=end {
        out.push_str(&format!(
            "{:>width$} │ {}\n",
            n,
            lines[n - 1],
            width = width
        ));
    }
    Ok(out)
}

/// Parse a 1-based inclusive line range: `"40-80"` → `(40, 80)`, `"12"` → `(12, 12)`.
pub fn parse_line_range(s: &str) -> Result<(usize, usize)> {
    let (a, b) = match s.split_once('-') {
        Some((a, b)) => (a.trim(), b.trim()),
        None => (s.trim(), s.trim()),
    };
    let start: usize = a
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid line range: {s}"))?;
    let end: usize = b
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid line range: {s}"))?;
    Ok((start, end))
}

/// Byte-exact body of the first symbol declaring `name`, signature through its end.
pub fn precise_symbol(content: &str, name: &str) -> Result<String> {
    let lines: Vec<&str> = content.lines().collect();
    let start = match lines
        .iter()
        .position(|l| is_signature_line(l.trim()) && declares(l.trim(), name))
    {
        Some(i) => i,
        None => bail!("symbol '{}' not found", name),
    };
    let end = symbol_end(&lines, start);
    let width = (end + 1).to_string().len();
    let mut out = String::new();
    for (i, line) in lines.iter().enumerate().take(end + 1).skip(start) {
        out.push_str(&format!("{:>width$} │ {}\n", i + 1, line, width = width));
    }
    Ok(out)
}

/// True if a signature line declares the identifier `name` (the token right after the
/// declaration keyword, e.g. `pub fn add` → `add`, `struct Point` → `Point`).
fn declares(trimmed: &str, name: &str) -> bool {
    const KEYWORDS: &[&str] = &[
        "fn",
        "def",
        "function",
        "func",
        "class",
        "struct",
        "enum",
        "trait",
        "interface",
        "type",
    ];
    let tokens: Vec<&str> = trimmed
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|s| !s.is_empty())
        .collect();
    tokens
        .windows(2)
        .any(|w| KEYWORDS.contains(&w[0]) && w[1] == name)
}

/// Find the last line index belonging to the symbol that starts at `start`.
/// Brace-based languages balance `{`/`}`; brace-free (Python) use indentation.
fn symbol_end(lines: &[&str], start: usize) -> usize {
    let mut depth: i32 = 0;
    let mut seen_brace = false;
    for (offset, line) in lines[start..].iter().enumerate() {
        for ch in line.chars() {
            if ch == '{' {
                depth += 1;
                seen_brace = true;
            } else if ch == '}' {
                depth -= 1;
            }
        }
        if seen_brace && depth <= 0 {
            return start + offset;
        }
    }
    if seen_brace {
        return lines.len() - 1; // unbalanced; fall back to EOF
    }
    // Brace-free: end before the next non-empty line at or above the signature's indent.
    let base = indent_of(lines[start]);
    for (offset, line) in lines[start + 1..].iter().enumerate() {
        if !line.trim().is_empty() && indent_of(line) <= base {
            return start + offset;
        }
    }
    lines.len() - 1
}

fn indent_of(line: &str) -> usize {
    line.chars().take_while(|c| *c == ' ' || *c == '\t').count()
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUST_SAMPLE: &str = "\
use std::fs;
use anyhow::Result;

/// Adds two numbers.
pub fn add(a: i32, b: i32) -> i32 {
    let sum = a + b;
    sum
}

struct Point {
    x: i32,
    y: i32,
}

fn helper() {
    println!(\"hi\");
}
";

    #[test]
    fn overview_code_keeps_signatures_and_imports_drops_bodies() {
        let ov = overview(RUST_SAMPLE, &Language::Rust, "sample.rs");
        assert!(ov.contains("use std::fs;"), "imports kept");
        assert!(ov.contains("pub fn add"), "fn signature kept");
        assert!(ov.contains("struct Point"), "struct signature kept");
        assert!(ov.contains("fn helper"), "all signatures kept");
        assert!(!ov.contains("let sum = a + b;"), "bodies dropped");
        assert!(!ov.contains("println!"), "bodies dropped");
    }

    #[test]
    fn overview_code_tags_original_line_numbers_and_drill_in() {
        let ov = overview(RUST_SAMPLE, &Language::Rust, "sample.rs");
        // add() is declared on line 5 of the sample.
        assert!(
            ov.lines()
                .any(|l| l.contains("pub fn add") && l.contains('5')),
            "add() must be tagged with original line 5:\n{ov}"
        );
        assert!(
            ov.contains("--symbol") && ov.contains("--lines"),
            "drill-in pointer present"
        );
    }

    #[test]
    fn overview_markdown_keeps_headings_drops_body() {
        let md = "# Title\n\nintro paragraph\n\n## Section A\n\nbody text\n\n### Sub\n";
        let ov = overview(md, &Language::Unknown, "doc.md");
        assert!(ov.contains("# Title"));
        assert!(ov.contains("## Section A"));
        assert!(ov.contains("### Sub"));
        assert!(!ov.contains("intro paragraph"), "prose body dropped");
        assert!(!ov.contains("body text"), "prose body dropped");
    }

    #[test]
    fn precise_lines_is_byte_exact_slice() {
        let lines: Vec<&str> = RUST_SAMPLE.lines().collect();
        let out = precise_lines(RUST_SAMPLE, 5, 8).unwrap();
        for n in 5..=8 {
            assert!(
                out.contains(lines[n - 1]),
                "line {n} verbatim: {:?}",
                lines[n - 1]
            );
        }
        assert!(!out.contains("fn helper"), "lines outside range excluded");
    }

    #[test]
    fn precise_lines_rejects_invalid_range() {
        assert!(precise_lines(RUST_SAMPLE, 0, 3).is_err(), "start < 1");
        assert!(precise_lines(RUST_SAMPLE, 5, 2).is_err(), "end < start");
        assert!(
            precise_lines(RUST_SAMPLE, 1, 9999).is_err(),
            "end beyond EOF"
        );
    }

    #[test]
    fn precise_symbol_returns_exact_body() {
        let out = precise_symbol(RUST_SAMPLE, "add").unwrap();
        assert!(out.contains("pub fn add(a: i32, b: i32) -> i32 {"));
        assert!(out.contains("let sum = a + b;"), "body included verbatim");
        assert!(out.contains("    sum"));
        assert!(
            !out.contains("fn helper"),
            "must not bleed into next symbol"
        );
    }

    #[test]
    fn precise_symbol_unknown_errs() {
        assert!(precise_symbol(RUST_SAMPLE, "nonexistent").is_err());
    }

    #[test]
    fn precise_symbol_finds_pub_crate_fn() {
        // pub(crate)/const/unsafe fns must be reachable, not silently missed.
        let src = "pub(crate) fn target(a: i32) -> i32 {\n    a + 1\n}\n";
        let out = precise_symbol(src, "target").unwrap();
        assert!(out.contains("pub(crate) fn target"));
        assert!(out.contains("a + 1"));
    }

    #[test]
    fn precise_symbol_python_indentation_body() {
        let py = "def add(a, b):\n    s = a + b\n    return s\n\ndef other():\n    pass\n";
        let out = precise_symbol(py, "add").unwrap();
        assert!(out.contains("def add(a, b):"));
        assert!(out.contains("    return s"), "indented body included");
        assert!(!out.contains("def other"), "stops at dedent");
    }

    #[test]
    fn parse_line_range_handles_pair_and_single() {
        assert_eq!(parse_line_range("40-80").unwrap(), (40, 80));
        assert_eq!(parse_line_range("12").unwrap(), (12, 12));
        assert!(parse_line_range("oops").is_err());
        assert!(parse_line_range("1-").is_err());
    }

    #[test]
    fn should_overview_thresholds_on_size() {
        assert!(!should_overview("small file"), "tiny file -> full");
        let big = "x\n".repeat(2000); // ~4000 chars -> ~1000 est tokens > 400
        assert!(should_overview(&big), "large file -> overview");
    }
}
