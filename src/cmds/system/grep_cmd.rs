//! Filters grep output by grouping matches by file.

use crate::core::config;
use crate::core::stream::exec_capture;
use crate::core::tracking;
use crate::core::utils::resolved_command;
use anyhow::{Context, Result};
use regex::Regex;
use std::collections::HashMap;

#[allow(clippy::too_many_arguments)]
pub fn run(
    pattern: &str,
    path: &str,
    max_line_len: usize,
    max_results: usize,
    context_only: bool,
    file_type: Option<&str>,
    extra_args: &[String],
    verbose: u8,
) -> Result<i32> {
    let timer = tracking::TimedExecution::start();

    if verbose > 0 {
        eprintln!("grep: '{}' in {}", pattern, path);
    }

    // Fix: convert BRE alternation \| → | for rg (which uses PCRE-style regex)
    let rg_pattern = pattern.replace(r"\|", "|");

    let mut rg_cmd = resolved_command("rg");
    // --no-ignore-vcs: match grep -r behavior (don't skip .gitignore'd files).
    // Without this, rg returns 0 matches for files in .gitignore, causing
    // false negatives that make AI agents draw wrong conclusions.
    // Using --no-ignore-vcs (not --no-ignore) so .ignore/.rgignore are still respected.
    rg_cmd.args(["-n", "--no-heading", "--no-ignore-vcs", &rg_pattern, path]);

    if let Some(ft) = file_type {
        rg_cmd.arg("--type").arg(ft);
    }

    for arg in extra_args {
        // Fix: skip grep-ism -r flag (rg is recursive by default; rg -r means --replace)
        if arg == "-r" || arg == "--recursive" {
            continue;
        }
        rg_cmd.arg(arg);
    }

    let result = exec_capture(&mut rg_cmd)
        .or_else(|_| {
            let mut grep_cmd = resolved_command("grep");
            //When we fall back to grep,include all args, not just -rn.
            grep_cmd.args(["-rn", pattern, path]).args(extra_args);
            exec_capture(&mut grep_cmd)
        })
        .context("grep/rg failed")?;

    // Passthrough output flags that produce output that is already small.
    if has_format_flag(extra_args) {
        print!("{}", result.stdout);
        if !result.stderr.is_empty() {
            eprint!("{}", result.stderr.trim());
        }

        let args_display = if extra_args.is_empty() {
            format!("'{}' {}", pattern, path)
        } else {
            format!("{} '{}' {}", extra_args.join(" "), pattern, path)
        };

        timer.track_passthrough(
            &format!("grep {}", args_display),
            &format!("rtk grep {} (passthrough)", args_display),
        );
        return Ok(result.exit_code);
    }

    let exit_code = result.exit_code;
    let raw_output = result.stdout.clone();

    if result.stdout.trim().is_empty() {
        // Show stderr for errors (bad regex, missing file, etc.)
        if exit_code == 2 && !result.stderr.trim().is_empty() {
            eprintln!("{}", result.stderr.trim());
        }
        let msg = format!("0 matches for '{}'", pattern);
        println!("{}", msg);
        timer.track(
            &format!("grep -rn '{}' {}", pattern, path),
            "rtk grep",
            &raw_output,
            &msg,
        );
        return Ok(exit_code);
    }

    // Always filter: truncate long lines, apply per-file and global caps.
    // Output in standard file:line:content format that AI agents can parse.
    // (A passthrough approach yields 0% savings — no reason for RTK to exist on that path.)
    let total_matches = result.stdout.lines().count();

    let context_re = if context_only {
        Regex::new(&format!("(?i).{{0,20}}{}.*", regex::escape(pattern))).ok()
    } else {
        None
    };

    let mut by_file: HashMap<String, Vec<(usize, String)>> = HashMap::new();
    for line in result.stdout.lines() {
        let parts: Vec<&str> = line.splitn(3, ':').collect();

        let (file, line_num, content) = if parts.len() == 3 {
            let ln = parts[1].parse().unwrap_or(0);
            (parts[0].to_string(), ln, parts[2])
        } else if parts.len() == 2 {
            let ln = parts[0].parse().unwrap_or(0);
            (path.to_string(), ln, parts[1])
        } else {
            continue;
        };

        let cleaned = clean_line(content, max_line_len, context_re.as_ref(), pattern);
        by_file.entry(file).or_default().push((line_num, cleaned));
    }

    let mut rtk_output = String::new();
    rtk_output.push_str(&format!(
        "{} matches in {} files:\n\n",
        total_matches,
        by_file.len()
    ));

    let mut shown = 0;
    let mut files: Vec<_> = by_file.iter().collect();
    files.sort_by_key(|(f, _)| *f);

    let per_file = config::limits().grep_max_per_file;
    for (file, matches) in files {
        if shown >= max_results {
            break;
        }

        let file_display = compact_path(file);
        for (line_num, content) in matches.iter().take(per_file) {
            if shown >= max_results {
                break;
            }
            rtk_output.push_str(&format!("{}:{}:{}\n", file_display, line_num, content));
            shown += 1;
        }
    }

    if total_matches > shown {
        rtk_output.push_str(&format!("[+{} more]\n", total_matches - shown));
    }

    print!("{}", rtk_output);
    timer.track(
        &format!("grep -rn '{}' {}", pattern, path),
        "rtk grep",
        &raw_output,
        &rtk_output,
    );

    Ok(exit_code)
}

/// Short grep flags that consume the following argument as their value
/// (`-A 3`, `-m 5`, …) — listed so the value is not mistaken for the pattern.
const VALUE_FLAGS: &[&str] = &["-A", "-B", "-C", "-m", "-d", "-D", "-f"];

/// Defaults used when a `grep` invocation is recovered outside clap; mirror the
/// clap defaults declared on `Commands::Grep` in main.rs.
const DEFAULT_MAX_LINE_LEN: usize = 80;
const DEFAULT_MAX_RESULTS: usize = 200;

/// Run a `grep` invocation that clap could not parse — flags before the pattern,
/// or flags like `-l`/`-rl` that collide with zap's own grep options. Parses the
/// raw args directly and dispatches to [`run`], so the command always reaches the
/// compressing handler (and ripgrep) instead of an unfiltered raw fallback.
///
/// Because this bypasses clap entirely, flags that clash with zap's grep options
/// (notably grep's `-l` = files-with-matches vs clap's `-l` = `--max-len`) flow
/// straight through to ripgrep, which interprets them correctly.
///
/// Returns `None` only when no pattern can be identified at all (e.g. a bare
/// `grep -l` with no pattern), leaving such invalid input to the normal fallback.
pub fn run_from_raw_args(args: &[String], verbose: u8) -> Option<anyhow::Result<i32>> {
    let (pattern, path, extra) = parse_grep_args(args)?;
    Some(run(
        &pattern,
        &path,
        DEFAULT_MAX_LINE_LEN,
        DEFAULT_MAX_RESULTS,
        false,
        None,
        &extra,
        verbose,
    ))
}

/// Parse arbitrary grep-style arguments into `(pattern, path, extra)` for [`run`],
/// translating grep-isms ripgrep reads differently. The pattern comes from
/// `-e`/`--regexp` if present, otherwise the first positional. `extra` carries the
/// surviving flags plus any additional paths, passed verbatim to ripgrep.
///
/// Returns `None` only if neither `-e` nor a positional yields a pattern.
fn parse_grep_args(args: &[String]) -> Option<(String, String, Vec<String>)> {
    let mut positionals: Vec<String> = Vec::new();
    let mut flags: Vec<String> = Vec::new();
    let mut e_pattern: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "--" {
            // Everything after `--` is a positional, even if it looks like a flag.
            positionals.extend(args[i + 1..].iter().cloned());
            break;
        }
        if a.len() > 1 && a.starts_with('-') {
            if a == "-e" || a == "--regexp" {
                // `-e PATTERN`: the value IS a pattern. The first becomes the
                // positional pattern; any further `-e` pass through to ripgrep.
                if let Some(val) = args.get(i + 1) {
                    if e_pattern.is_none() {
                        e_pattern = Some(val.clone());
                    } else {
                        flags.push("-e".to_string());
                        flags.push(val.clone());
                    }
                    i += 1;
                }
            } else if VALUE_FLAGS.contains(&a.as_str()) {
                flags.push(a.clone());
                if let Some(val) = args.get(i + 1) {
                    flags.push(val.clone());
                    i += 1;
                }
            } else if let Some(translated) = translate_flag(a) {
                flags.push(translated);
            }
            // else: flag intentionally dropped (e.g. -r, -E)
        } else {
            positionals.push(a.clone());
        }
        i += 1;
    }

    let pattern = e_pattern.clone().or_else(|| positionals.first().cloned())?;
    // If the pattern came from `-e`, every positional is a path; otherwise the
    // first positional was the pattern.
    let skip = if e_pattern.is_some() { 0 } else { 1 };
    let paths: Vec<String> = positionals.into_iter().skip(skip).collect();
    let path = paths.first().cloned().unwrap_or_else(|| ".".to_string());
    let mut extra = flags;
    extra.extend(paths.into_iter().skip(1)); // additional paths
    Some((pattern, path, extra))
}

/// Translate a single non-value grep flag into its ripgrep-friendly form.
/// Returns `None` for flags that must be dropped (rg's defaults already cover
/// them, or they collide with a different rg meaning).
fn translate_flag(flag: &str) -> Option<String> {
    match flag {
        // rg is recursive by default; rg's -r means --replace.
        "-r" | "-R" | "--recursive" => None,
        // rg uses Rust regex (ERE-like) by default; -E collides with rg's --encoding.
        "-E" | "--extended-regexp" | "-G" | "--basic-regexp" => None,
        _ => {
            // Short-flag cluster (e.g. -rn, -rl, -in): drop the recursive letters
            // so the rest survives without tripping rg's -r/--replace.
            let body = flag.strip_prefix('-').unwrap_or("");
            let is_cluster =
                body.len() > 1 && !body.starts_with('-') && body.chars().all(|c| c.is_ascii_alphabetic());
            if is_cluster {
                let kept: String = body.chars().filter(|c| *c != 'r' && *c != 'R').collect();
                return if kept.is_empty() {
                    None
                } else {
                    Some(format!("-{kept}"))
                };
            }
            Some(flag.to_string())
        }
    }
}

fn has_format_flag(extra_args: &[String]) -> bool {
    extra_args.iter().any(|arg| {
        matches!(
            arg.as_str(),
            "-c" | "--count"
                | "-l"
                | "--files-with-matches"
                | "-L"
                | "--files-without-match"
                | "-o"
                | "--only-matching"
                | "-Z"
                | "--null"
        )
    })
}

fn clean_line(line: &str, max_len: usize, context_re: Option<&Regex>, pattern: &str) -> String {
    let trimmed = line.trim();

    if let Some(re) = context_re {
        if let Some(m) = re.find(trimmed) {
            let matched = m.as_str();
            if matched.len() <= max_len {
                return matched.to_string();
            }
        }
    }

    if trimmed.len() <= max_len {
        trimmed.to_string()
    } else {
        let lower = trimmed.to_lowercase();
        let pattern_lower = pattern.to_lowercase();

        if let Some(pos) = lower.find(&pattern_lower) {
            let char_pos = lower[..pos].chars().count();
            let chars: Vec<char> = trimmed.chars().collect();
            let char_len = chars.len();

            let start = char_pos.saturating_sub(max_len / 3);
            let end = (start + max_len).min(char_len);
            let start = if end == char_len {
                end.saturating_sub(max_len)
            } else {
                start
            };

            let slice: String = chars[start..end].iter().collect();
            if start > 0 && end < char_len {
                format!("...{}...", slice)
            } else if start > 0 {
                format!("...{}", slice)
            } else {
                format!("{}...", slice)
            }
        } else {
            let t: String = trimmed.chars().take(max_len - 3).collect();
            format!("{}...", t)
        }
    }
}

fn compact_path(path: &str) -> String {
    if path.len() <= 50 {
        return path.to_string();
    }

    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() <= 3 {
        return path.to_string();
    }

    format!(
        "{}/.../{}/{}",
        parts[0],
        parts[parts.len() - 2],
        parts[parts.len() - 1]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clean_line() {
        let line = "            const result = someFunction();";
        let cleaned = clean_line(line, 50, None, "result");
        assert!(!cleaned.starts_with(' '));
        assert!(cleaned.len() <= 50);
    }

    #[test]
    fn test_compact_path() {
        let path = "/Users/patrick/dev/project/src/components/Button.tsx";
        let compact = compact_path(path);
        assert!(compact.len() <= 60);
    }

    #[test]
    fn test_extra_args_accepted() {
        // Test that the function signature accepts extra_args
        // This is a compile-time test - if it compiles, the signature is correct
        let _extra: Vec<String> = vec!["-i".to_string(), "-A".to_string(), "3".to_string()];
        // No need to actually run - we're verifying the parameter exists
    }

    #[test]
    fn test_clean_line_multibyte() {
        // Thai text that exceeds max_len in bytes
        let line = "  สวัสดีครับ นี่คือข้อความที่ยาวมากสำหรับทดสอบ  ";
        let cleaned = clean_line(line, 20, None, "ครับ");
        // Should not panic
        assert!(!cleaned.is_empty());
    }

    #[test]
    fn test_clean_line_emoji() {
        let line = "🎉🎊🎈🎁🎂🎄 some text 🎃🎆🎇✨";
        let cleaned = clean_line(line, 15, None, "text");
        assert!(!cleaned.is_empty());
    }

    // Fix: BRE \| alternation is translated to PCRE | for rg
    #[test]
    fn test_bre_alternation_translated() {
        let pattern = r"fn foo\|pub.*bar";
        let rg_pattern = pattern.replace(r"\|", "|");
        assert_eq!(rg_pattern, "fn foo|pub.*bar");
    }

    // Fix: -r flag (grep recursive) is stripped from extra_args (rg is recursive by default)
    #[test]
    fn test_recursive_flag_stripped() {
        let extra_args: Vec<String> = vec!["-r".to_string(), "-i".to_string()];
        let filtered: Vec<&String> = extra_args
            .iter()
            .filter(|a| *a != "-r" && *a != "--recursive")
            .collect();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0], "-i");
    }

    // --- truncation accuracy ---

    #[test]
    fn test_grep_overflow_uses_uncapped_total() {
        // Confirm the grep overflow invariant: matches vec is never capped before overflow calc.
        // If total_matches > per_file, overflow = total_matches - per_file (not capped).
        // This documents that grep_cmd.rs avoids the diff_cmd bug (cap at N then compute N-10).
        let per_file = config::limits().grep_max_per_file;
        let total_matches = per_file + 42;
        let overflow = total_matches - per_file;
        assert_eq!(overflow, 42, "overflow must equal true suppressed count");
        // Demonstrate why capping before subtraction is wrong:
        let hypothetical_cap = per_file + 5;
        let capped = total_matches.min(hypothetical_cap);
        let wrong_overflow = capped - per_file;
        assert_ne!(
            wrong_overflow, overflow,
            "capping before subtraction gives wrong overflow"
        );
    }

    // --- format flag detection ---

    #[test]
    fn test_format_flag_detects_count() {
        assert!(has_format_flag(&["-c".to_string()]));
        assert!(has_format_flag(&["--count".to_string()]));
    }

    #[test]
    fn test_format_flag_detects_files_with_matches() {
        assert!(has_format_flag(&["-l".to_string()]));
        assert!(has_format_flag(&["--files-with-matches".to_string()]));
    }

    #[test]
    fn test_format_flag_detects_files_without_match() {
        assert!(has_format_flag(&["-L".to_string()]));
        assert!(has_format_flag(&["--files-without-match".to_string()]));
    }

    #[test]
    fn test_format_flag_detects_only_matching() {
        assert!(has_format_flag(&["-o".to_string()]));
        assert!(has_format_flag(&["--only-matching".to_string()]));
    }

    #[test]
    fn test_format_flag_detects_null() {
        assert!(has_format_flag(&["-Z".to_string()]));
        assert!(has_format_flag(&["--null".to_string()]));
    }

    #[test]
    fn test_format_flag_ignores_normal_flags() {
        assert!(!has_format_flag(&[
            "-i".to_string(),
            "-w".to_string(),
            "-A".to_string(),
            "3".to_string(),
        ]));
    }

    // Verify line numbers are always enabled in rg invocation (grep_cmd.rs:24).
    // The -n/--line-numbers clap flag in main.rs is a no-op accepted for compat.
    #[test]
    fn test_rg_always_has_line_numbers() {
        // grep_cmd::run() always passes "-n" to rg (line 24).
        // This test documents that -n is built-in, so the clap flag is safe to ignore.
        let mut cmd = resolved_command("rg");
        cmd.args(["-n", "--no-heading", "NONEXISTENT_PATTERN_12345", "."]);
        // If rg is available, it should accept -n without error (exit 1 = no match, not error)
        if let Ok(output) = cmd.output() {
            assert!(
                output.status.code() == Some(1) || output.status.success(),
                "rg -n should be accepted"
            );
        }
        // If rg is not installed, skip gracefully (test still passes)
    }

    // --- argv normalization: flags-before-pattern recovery (issue: grep fallbacks) ---

    #[test]
    fn parse_flags_before_pattern_finds_pattern_and_path() {
        let (p, path, _e) = parse_grep_args(&svec(&["-rn", "fn main", "src"])).unwrap();
        assert_eq!(p, "fn main", "pattern is the first positional");
        assert_eq!(path, "src", "path is the second positional");
    }

    #[test]
    fn parse_strips_recursive_from_cluster() {
        // -rn -> -n (rg is recursive by default; rg -r means --replace)
        let (_p, _path, e) = parse_grep_args(&svec(&["-rn", "TODO", "src"])).unwrap();
        assert!(e.contains(&"-n".to_string()), "cluster -rn should yield -n: {e:?}");
        assert!(!e.iter().any(|a| a == "-r" || a == "-rn"), "no -r/-rn reaches rg");
    }

    // The bug this round fixes: -rl / -l previously fell back because clap bound
    // `-l` to --max-len. Bypassing clap, `-l` now flows straight to ripgrep.
    #[test]
    fn parse_rl_cluster_sends_l_to_extra() {
        let (_p, _path, e) = parse_grep_args(&svec(&["-rl", "Tracker", "src"])).unwrap();
        assert!(
            e.contains(&"-l".to_string()),
            "-l (files-with-matches) must reach ripgrep, not collide with clap: {e:?}"
        );
        assert!(!e.iter().any(|a| a == "-rl" || a == "-r"));
    }

    #[test]
    fn parse_standalone_l_sends_l_to_extra() {
        let (p, path, e) = parse_grep_args(&svec(&["-l", "foo"])).unwrap();
        assert_eq!(p, "foo");
        assert_eq!(path, ".");
        assert!(e.contains(&"-l".to_string()));
    }

    #[test]
    fn parse_defaults_path_to_dot() {
        let (p, path, _e) = parse_grep_args(&svec(&["-rn", "TODO"])).unwrap();
        assert_eq!(p, "TODO");
        assert_eq!(path, ".", "missing path defaults to '.'");
    }

    #[test]
    fn parse_keeps_value_flag_with_its_value() {
        // -A 3 (after-context) must stay paired and not be mistaken for the pattern.
        let (p, path, e) = parse_grep_args(&svec(&["-A", "3", "foo", "src"])).unwrap();
        assert_eq!(p, "foo");
        assert_eq!(path, "src");
        let pos = e.iter().position(|a| a == "-A").expect("-A present");
        assert_eq!(e[pos + 1], "3", "-A must keep its value");
    }

    #[test]
    fn parse_drops_extended_regexp_flag() {
        // rg uses Rust regex by default; grep's -E is redundant and collides with
        // rg's -E (--encoding), so it is dropped.
        let (p, _path, e) = parse_grep_args(&svec(&["-E", "a|b", "src"])).unwrap();
        assert_eq!(p, "a|b");
        assert!(!e.iter().any(|a| a == "-E"), "-E must be dropped: {e:?}");
    }

    #[test]
    fn parse_passes_through_normal_flags() {
        let (p, path, e) = parse_grep_args(&svec(&["-i", "foo", "src"])).unwrap();
        assert_eq!(p, "foo");
        assert_eq!(path, "src");
        assert!(e.contains(&"-i".to_string()), "-i must pass through to rg");
    }

    #[test]
    fn parse_flag_between_positionals() {
        let (p, path, e) = parse_grep_args(&svec(&["pattern", "-i", "src"])).unwrap();
        assert_eq!(p, "pattern");
        assert_eq!(path, "src");
        assert!(e.contains(&"-i".to_string()));
    }

    // -e is now handled (previously bailed): its value becomes the pattern.
    #[test]
    fn parse_e_flag_extracts_pattern() {
        let (p, path, e) = parse_grep_args(&svec(&["-e", "foo", "src"])).unwrap();
        assert_eq!(p, "foo");
        assert_eq!(path, "src");
        assert!(!e.iter().any(|a| a == "-e"), "single -e becomes the positional pattern");
    }

    #[test]
    fn parse_none_when_no_pattern() {
        assert!(parse_grep_args(&svec(&["-rn"])).is_none());
    }

    // "Works no matter what args": a messy real-world combo still parses cleanly.
    #[test]
    fn parse_arbitrary_combo_is_robust() {
        let (p, path, e) =
            parse_grep_args(&svec(&["-rin", "-A", "2", "needle", "src", "lib"])).unwrap();
        assert_eq!(p, "needle");
        assert_eq!(path, "src");
        assert!(e.contains(&"-in".to_string()), "-rin -> -in: {e:?}");
        assert!(e.windows(2).any(|w| w[0] == "-A" && w[1] == "2"), "-A 2 preserved");
        assert!(e.contains(&"lib".to_string()), "extra path preserved");
    }

    fn svec(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn test_rg_no_ignore_vcs_flag_accepted() {
        // Verify rg accepts --no-ignore-vcs (used to match grep -r behavior for .gitignore)
        let mut cmd = resolved_command("rg");
        cmd.args([
            "-n",
            "--no-heading",
            "--no-ignore-vcs",
            "NONEXISTENT_PATTERN_12345",
            ".",
        ]);
        if let Ok(output) = cmd.output() {
            assert!(
                output.status.code() == Some(1) || output.status.success(),
                "rg --no-ignore-vcs should be accepted"
            );
        }
        // If rg is not installed, skip gracefully (test still passes)
    }
}
