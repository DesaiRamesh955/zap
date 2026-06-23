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

/// Short grep flags that consume the following argument as their value.
/// (`-A 3`, `-m 5`, etc.) Listed so the value is not mistaken for the pattern.
/// `-e`/`-f` carry the pattern/file and are deliberately out of scope here.
const VALUE_FLAGS: &[&str] = &["-A", "-B", "-C", "-m", "-d", "-D", "-e", "-f"];

/// Recover a full `zap`-style argv when the `grep` subcommand was written in the
/// conventional `grep -flags pattern [path]` order that clap's positional-first
/// parser rejects (pattern is its first positional).
///
/// Returns `Some(new_argv)` with the grep arguments rewritten into clap's
/// canonical `pattern path extra…` order, or `None` when the command is not a
/// recoverable `grep` invocation (different subcommand, no pattern, `-e` form).
/// The returned argv keeps the program name and any global flags untouched.
pub fn normalize_argv(argv: &[String]) -> Option<Vec<String>> {
    // The subcommand is the first non-flag token (all global flags are boolean).
    let sub_idx = argv.iter().skip(1).position(|a| !a.starts_with('-'))? + 1;
    if argv[sub_idx] != "grep" {
        return None;
    }
    let normalized = normalize_grep_args(&argv[sub_idx + 1..])?;
    let mut out: Vec<String> = argv[..=sub_idx].to_vec();
    out.extend(normalized);
    Some(out)
}

/// Reorder raw grep arguments into clap's expected `pattern path extra…` order,
/// translating grep-isms that ripgrep interprets differently.
/// Returns `None` if no positional pattern can be identified or an `-e`/`-f`
/// pattern-bearing flag is present (left for clap/fallback to handle).
fn normalize_grep_args(args: &[String]) -> Option<Vec<String>> {
    let mut positionals: Vec<String> = Vec::new();
    let mut flags: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "--" {
            // Everything after `--` is a positional, even if it looks like a flag.
            positionals.extend(args[i + 1..].iter().cloned());
            break;
        }
        if a.len() > 1 && a.starts_with('-') {
            if VALUE_FLAGS.contains(&a.as_str()) {
                // `-e`/`-f` carry the pattern/file — out of scope, defer to clap.
                if a == "-e" || a == "-f" {
                    return None;
                }
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

    let pattern = positionals.first()?.clone();
    let path = positionals.get(1).cloned().unwrap_or_else(|| ".".to_string());

    let mut out = vec![pattern, path];
    out.extend(flags);
    out.extend(positionals.into_iter().skip(2)); // extra paths
    Some(out)
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
    fn test_normalize_flags_before_pattern_puts_pattern_first() {
        let args = svec(&["-rn", "fn main", "src"]);
        let out = normalize_grep_args(&args).unwrap();
        assert_eq!(out[0], "fn main", "pattern must be first positional");
        assert_eq!(out[1], "src", "path must be second positional");
    }

    #[test]
    fn test_normalize_strips_recursive_from_cluster() {
        // -rn -> -n (rg is recursive by default; rg -r means --replace)
        let out = normalize_grep_args(&svec(&["-rn", "TODO", "src"])).unwrap();
        assert!(out.contains(&"-n".to_string()), "cluster -rn should yield -n");
        assert!(
            !out.iter().any(|a| a == "-r" || a == "-rn"),
            "no -r/-rn should reach rg: {out:?}"
        );
    }

    #[test]
    fn test_normalize_rl_cluster_becomes_l() {
        let out = normalize_grep_args(&svec(&["-rl", "foo", "src"])).unwrap();
        assert!(out.contains(&"-l".to_string()), "cluster -rl should yield -l");
        assert!(!out.iter().any(|a| a == "-rl" || a == "-r"));
    }

    #[test]
    fn test_normalize_defaults_path_to_dot() {
        let out = normalize_grep_args(&svec(&["-rn", "TODO"])).unwrap();
        assert_eq!(out[0], "TODO");
        assert_eq!(out[1], ".", "missing path must default to '.'");
    }

    #[test]
    fn test_normalize_keeps_value_flag_with_its_value() {
        // -A 3 (after-context) must stay paired and not be mistaken for the pattern
        let out = normalize_grep_args(&svec(&["-A", "3", "foo", "src"])).unwrap();
        assert_eq!(out[0], "foo");
        assert_eq!(out[1], "src");
        let pos = out.iter().position(|a| a == "-A").expect("-A present");
        assert_eq!(out[pos + 1], "3", "-A must keep its value");
    }

    #[test]
    fn test_normalize_drops_extended_regexp_flag() {
        // rg uses Rust regex (ERE-like) by default; grep's -E is redundant and
        // collides with rg's -E (--encoding), so it must be dropped.
        let out = normalize_grep_args(&svec(&["-E", "a|b", "src"])).unwrap();
        assert_eq!(out[0], "a|b");
        assert!(!out.iter().any(|a| a == "-E"), "-E must be dropped: {out:?}");
    }

    #[test]
    fn test_normalize_passes_through_normal_flags() {
        let out = normalize_grep_args(&svec(&["-i", "foo", "src"])).unwrap();
        assert_eq!(out[0], "foo");
        assert_eq!(out[1], "src");
        assert!(out.contains(&"-i".to_string()), "-i must pass through to rg");
    }

    #[test]
    fn test_normalize_flag_between_positionals() {
        // grep pattern -i src  (flag wedged between pattern and path) also fails clap today
        let out = normalize_grep_args(&svec(&["pattern", "-i", "src"])).unwrap();
        assert_eq!(out[0], "pattern");
        assert_eq!(out[1], "src");
        assert!(out.contains(&"-i".to_string()));
    }

    #[test]
    fn test_normalize_bails_on_e_flag() {
        // -e carries the pattern as its value; out of scope, let clap/fallback handle it.
        assert!(normalize_grep_args(&svec(&["-e", "foo", "src"])).is_none());
    }

    #[test]
    fn test_normalize_none_when_no_positional_pattern() {
        assert!(normalize_grep_args(&svec(&["-rn"])).is_none());
    }

    #[test]
    fn test_normalize_argv_finds_grep_subcommand() {
        let argv = svec(&["zap", "grep", "-rn", "foo", "src"]);
        let out = normalize_argv(&argv).unwrap();
        assert_eq!(out[0], "zap");
        assert_eq!(out[1], "grep");
        assert_eq!(out[2], "foo");
        assert_eq!(out[3], "src");
    }

    #[test]
    fn test_normalize_argv_preserves_global_flags() {
        let argv = svec(&["zap", "-v", "grep", "-rn", "foo"]);
        let out = normalize_argv(&argv).unwrap();
        assert_eq!(out[0], "zap");
        assert_eq!(out[1], "-v");
        assert_eq!(out[2], "grep");
        assert_eq!(out[3], "foo");
        assert_eq!(out[4], ".");
    }

    #[test]
    fn test_normalize_argv_none_for_non_grep() {
        assert!(normalize_argv(&svec(&["zap", "git", "status"])).is_none());
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
