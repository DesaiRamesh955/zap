# Zap — Token-Optimized CLI Proxy (Codex CLI)

**Usage**: Token-optimized CLI proxy for shell commands.

## Rule

Always prefix shell commands with `zap`.

Examples:

```bash
zap git status
zap cargo test
zap npm run build
zap pytest -q
```

## Meta Commands

```bash
zap gain            # Token savings analytics
zap gain --history  # Recent command savings history
zap proxy <cmd>     # Run raw command without filtering
```

## Verification

```bash
zap --version
zap gain
which zap
```
