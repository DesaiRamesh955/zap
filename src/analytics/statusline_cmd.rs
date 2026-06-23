//! `zap statusline` — one honest line for the Claude Code status bar.
//!
//! Shows real tokens saved today (an estimate, labeled `~`) plus the fallback count,
//! so a zero-savings or fell-back run is visible rather than hidden. Designed to be
//! instant (two indexed `COUNT`/`SUM` queries) and to never break the status bar.

use crate::core::tracking::Tracker;
use anyhow::Result;

/// Render the status line and print it to stdout.
pub fn run(_verbose: u8) -> Result<()> {
    // Claude Code feeds session JSON on stdin; we don't need it, but draining avoids
    // a broken-pipe on the writer's side.
    drain_stdin();

    let line = match Tracker::new().and_then(|t| t.get_statusline_stats()) {
        Ok((saved, fallbacks)) => format_statusline(saved, fallbacks),
        // Never break the status bar: degrade to a bare badge on any DB error.
        Err(_) => "⚡ zap".to_string(),
    };
    println!("{line}");
    Ok(())
}

fn drain_stdin() {
    use std::io::Read;
    let mut buf = String::new();
    let _ = std::io::stdin().read_to_string(&mut buf);
}

/// Build the honest status line. The token figure is labeled `~` (it is an estimate,
/// `chars/4`); the fallback clause appears only when there is something to report.
fn format_statusline(saved: i64, fallbacks: i64) -> String {
    let mut s = format!("⚡ zap ~{} saved today", fmt_tokens(saved));
    if fallbacks > 0 {
        s.push_str(&format!(
            " · {} fallback{}",
            fallbacks,
            if fallbacks == 1 { "" } else { "s" }
        ));
    }
    s
}

/// Compact a token count: `1234 -> "1.2k"`, `999 -> "999"`.
fn fmt_tokens(n: i64) -> String {
    if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shows_saved_estimate_with_tilde() {
        let s = format_statusline(12340, 0);
        assert!(
            s.contains('~'),
            "token figure must be labeled an estimate: {s}"
        );
        assert!(s.contains("12.3k"));
        assert!(s.contains("saved today"));
    }

    #[test]
    fn shows_fallbacks_when_present() {
        assert!(format_statusline(500, 2).contains("2 fallbacks"));
    }

    #[test]
    fn singular_fallback_is_not_pluralized() {
        let s = format_statusline(0, 1);
        assert!(s.contains("1 fallback"));
        assert!(!s.contains("1 fallbacks"));
    }

    #[test]
    fn hides_fallback_clause_when_zero() {
        assert!(!format_statusline(100, 0).contains("fallback"));
    }

    #[test]
    fn fmt_tokens_compacts_thousands() {
        assert_eq!(fmt_tokens(534), "534");
        assert_eq!(fmt_tokens(1500), "1.5k");
    }
}
