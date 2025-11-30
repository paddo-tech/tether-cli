# AGENTS.md

> `CLAUDE.md` is symlinked to this file.

## Overview

Rust CLI that syncs dotfiles and global packages across machines via Git. Daemon runs periodic sync every 5 minutes.

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
│   ├── commands/
│   │   ├── mod.rs       # CLI parsing + dispatch
│   │   ├── init.rs      # tether init
│   │   ├── sync.rs      # tether sync
│   │   ├── status.rs    # tether status
│   │   ├── diff.rs      # tether diff
│   │   ├── config.rs    # tether config
│   │   ├── daemon.rs    # tether daemon
│   │   ├── machines.rs  # tether machines
│   │   ├── ignore.rs    # tether ignore
│   │   └── team.rs      # tether team
│   ├── output.rs        # Terminal formatting
│   └── prompts.rs       # Interactive prompts
├── config.rs            # Config management
├── daemon/
│   └── server.rs        # Background daemon (periodic sync)
├── github.rs            # GitHub repo creation via gh CLI
├── packages/
│   ├── manager.rs       # PackageManager trait
│   ├── brew.rs, npm.rs, pnpm.rs, bun.rs, gem.rs
├── security/
│   ├── encryption.rs    # AES-GCM encryption
│   ├── keychain.rs      # Key management (passphrase-based)
│   └── secrets.rs       # Secret detection
├── sync/
│   ├── engine.rs        # sync_path() helper
│   ├── git.rs           # Git operations
│   ├── state.rs         # State tracking
│   └── team.rs          # Team sync
└── lib.rs
```

## Key Dependencies

- **clap** - CLI parsing
- **tokio** - Async runtime
- **git2** - Git operations
- **inquire** - Interactive prompts
- **owo-colors** - Terminal colors
- **aes-gcm** - Encryption
- **age** - Passphrase-based key encryption

## Data Layout

`~/.tether/`: config.toml, state.json, sync/, daemon.pid, daemon.log, ignore

Sync repo: dotfiles/, configs/, manifests/, machines/, projects/

## Code Quality

Before completing work:
1. `cargo clippy --all-targets -- -D warnings` (zero warnings)
2. `cargo fmt --all`
3. `cargo build --release`
