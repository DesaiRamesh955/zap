# Zap — Token-Optimized CLI Proxy

**Usage**: Token-optimized CLI proxy (60-90% savings on dev operations)

## Meta Commands (always use zap directly)

```bash
zap gain              # Show token savings analytics
zap gain --history    # Show command usage history with savings
zap discover          # Analyze Claude Code history for missed opportunities
zap proxy <cmd>       # Execute raw command without filtering (for debugging)
```

## Installation Verification

```bash
zap --version         # Should show: zap X.Y.Z
zap gain              # Should work (not "command not found")
which zap             # Verify correct binary
```

⚠️ **Wrong binary?** If `zap gain` fails or shows a different tool, make sure `~/.cargo/bin/zap` is first on your PATH (`which zap`).

## Hook-Based Usage

All other commands are automatically rewritten by the Claude Code hook.
Example: `git status` → `zap git status` (transparent, 0 tokens overhead)

Refer to CLAUDE.md for full command reference.
