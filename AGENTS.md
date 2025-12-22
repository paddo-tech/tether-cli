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

## Releasing

Homebrew tap: `paddo-tech/homebrew-tap`

### Version Bump Checklist

1. Update version in `Cargo.toml`
2. Add entry to `CHANGELOG.md` with date and changes
3. Commit: `git commit -am "chore: release vX.Y.Z"`
4. Push to main - CI creates tag, builds, signs, notarizes, and updates Homebrew formula

**Do NOT create tags manually** - the release workflow handles tagging.

### Versioning

- **Patch** (1.0.x): Bug fixes
- **Minor** (1.x.0): New features, backward compatible
- **Major** (x.0.0): Breaking changes
- **Prerelease**: Use `-beta.N` suffix (creates versioned formula)

Users install via:
```bash
brew tap paddo-tech/tap
brew install tether
```
