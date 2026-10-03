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

## CLI Commands

| Command | Description |
|---------|-------------|
| `init` | Initialize Tether on this machine |
| `sync` | Manually trigger a sync |
| `status` | Show current sync status |
| `diff` | Show differences between machines |
| `config` | Manage configuration and feature toggles |
| `daemon` | Control the background daemon |
| `machines` | Manage machines in sync network |
| `ignore` | Manage ignore patterns |
| `team` | Manage team sync (dotfiles, secrets, projects) |
| `resolve` | Resolve file conflicts |
| `unlock` | Unlock encryption key with passphrase |
| `lock` | Clear cached encryption key |
| `upgrade` | Upgrade all installed packages |
| `restore` | Restore files from backup |
| `identity` | Manage age identity for team secrets |
| `collab` | Collaborator-based project secret sharing |

## Key Dependencies

- **clap** - CLI parsing
- **tokio** - Async runtime
- **git2** - Git operations
- **inquire** - Interactive prompts
- **owo-colors** - Terminal colors
- **aes-gcm** - Encryption
- **age** - Passphrase-based key encryption
- **ssh-key** - Commit signing keys and SSH signatures

## Feature Toggles

Managed via `tether config features`. Available toggles:

| Feature | Default | Description |
|---------|---------|-------------|
| `personal_dotfiles` | true | Sync personal dotfiles |
| `personal_packages` | true | Sync personal package manifests |
| `team_dotfiles` | false | Sync team dotfiles |
| `collab_secrets` | false | Enable collab secret sharing |
| `team_layering` | false | Merge team + personal dotfiles |

## Data Layout

**~/.tether/**
- `config.toml` - Main config (versioned)
- `local.toml` - Settings for this machine only (never synced). `[packages] min_release_age_days` and `[merge] command`/`args` override config.toml on this machine
- `state.json` - Sync state, including `install_failures` (synced packages that failed to install here; retried after 24h or on a new version)
- `sync/` - Personal sync repo
- `teams/<name>/` - Team sync repos
- `collabs/` - Collab project configs
- `identity.pub` - Age public key
- `daemon.pid` - Daemon process ID
- `daemon.log` - Daemon logs
- `backups/` - File backups
- `conflicts.json` - Conflict state
- `inbox.json` - Approval inbox (never synced)
- `inbox.lock` - Lock for inbox and trust store changes
- `machine.json` - This machine's last record (never synced). Its removals and ignores carry into the next record; the repo copy is never read for them
- `record_generations.json` - Newest record `generation` accepted per machine key fingerprint, with the SHA-256 of that record (never synced). A signed record from the same key with a lower generation, or a different record with the same generation, is a replay and grants no trust
- `run/` - Empty working directory for package managers
- `signing_key` - This machine's ed25519 SSH key for record and commit signatures (0600)
- `trusted_keys` - Trusted machine keys (never synced). TOML: `version = 1`, then `[machines."<id>"]` with `public_key` (OpenSSH line) and `fingerprint` (`SHA256:...`). Earlier builds wrote git allowed signers lines; Tether reads them and rewrites the file as TOML on the next trust change

**Sync repo structure:**
- `dotfiles/` - Dotfiles
- `configs/` - App configs
- `manifests/` - Package manifests
- `machines/` - Machine-specific state (`<id>.json`) and its signature (`<id>.json.sig`, sshsig, namespace `tether-machine`, over `tether-machine-v1\n<id>\n<sha256 of the json bytes>\n`). Only a record whose signature verifies against the trusted key for its id lets packages auto-install. Trust is transitive: a signed record lists the packages its machine installed, so trusting a machine trusts what it installed. A local rejection still blocks a package
- `projects/` - Project secrets

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
