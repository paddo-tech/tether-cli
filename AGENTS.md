# AGENTS.md

> `CLAUDE.md` is symlinked to this file.

## Overview

Rust CLI that syncs dotfiles and global packages across Macs via Git. Background daemon watches for changes.

## Commands

```bash
cargo build              # Build
cargo run -- <cmd>       # Run in dev
cargo test               # Test
cargo clippy -- -D warnings  # Lint (must pass before commits)
cargo fmt                # Format
```

## Source Structure

```
src/
├── cli/
│   ├── commands/mod.rs  # All commands (init, sync, status, diff, daemon, machines, ignore, config, team)
│   ├── output.rs        # Terminal formatting
│   └── prompts.rs       # Interactive prompts
├── config.rs            # Config management
├── daemon/
│   ├── server.rs        # Background process
│   └── ipc.rs           # Unix socket IPC
├── github.rs            # GitHub repo creation via gh CLI
├── packages/
│   ├── manager.rs       # PackageManager trait
│   ├── brew.rs          # Homebrew
│   ├── npm.rs           # npm globals
│   ├── pnpm.rs          # pnpm globals
│   ├── bun.rs           # Bun globals
│   └── gem.rs           # Ruby gems
├── security/
│   ├── encryption.rs    # AES-GCM encryption
│   ├── keychain.rs      # macOS Keychain
│   └── secrets.rs       # Secret detection/scanning
├── sync/
│   ├── engine.rs        # Core sync logic
│   ├── git.rs           # Git operations
│   ├── conflict.rs      # Conflict resolution
│   ├── state.rs         # State tracking
│   └── team.rs          # Team sync (shared configs)
└── watcher.rs           # File watching (notify crate)
```

## Key Dependencies

- **clap** - CLI parsing
- **tokio** - Async runtime
- **git2** - Git operations
- **notify** - File watching
- **inquire** - Interactive prompts
- **owo-colors** - Terminal colors
- **aes-gcm** - Encryption
- **security-framework** - macOS Keychain

## Data Layout

`~/.tether/`:
- `config.toml` - User config
- `state.json` - Sync state
- `sync/` - Git repo clone
- `daemon.pid`, `daemon.log`

Sync repo:
- `dotfiles/` - Synced dotfiles
- `configs/` - Config directories
- `manifests/` - Package manifests (brew.json, npm.json, etc.)
- `machines/` - Machine metadata
- `projects/` - Project-local configs

## Code Quality

Before completing work:
1. `cargo clippy --all-targets -- -D warnings` (zero warnings)
2. `cargo fmt --all`
3. `cargo build --release`

## Notes

- macOS only (v1.0)
- Uses Git as sync backend
- Secret scanning prevents syncing credentials
- Daemon uses launchd
