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
- `manifests/` - Package manifests: one package name per line, never a version, as 1.x reads them. Versions come only from signed machine records (`package_versions`). Pinned lines from pre-release 2.0 builds still parse, and the next write drops the version
- `machines/` - Machine-specific state (`<id>.json`) and its signature (`<id>.json.sig`, sshsig, namespace `tether-machine`, over `tether-machine-v1\n<id>\n<sha256 of the json bytes>\n`). Only a record whose signature verifies against the trusted key for its id lets packages auto-install. Trust is transitive: a signed record lists the packages its machine installed, so trusting a machine trusts what it installed. A local rejection still blocks a package
- `projects/` - Project secrets

## Compatibility with 1.x

- Rolling upgrades work one machine at a time. A sync repo can hold 1.x and 2.0 machines together.
- The 2.0 protections (trust, release age, OSV, signatures) apply only on upgraded machines. A 1.x machine installs without them. Its unsigned record grants no trust, so its packages wait in the inbox of 2.0 machines.
- Manifests stay names-only. Never write `name@ver`, `name==ver` or `name:ver` to `manifests/*`.
- Synced formats change only by additions until 3.0. Do not add `deny_unknown_fields` to a synced struct. Do not remove, rename or retype a field that 1.11.10, 1.12.0 or 1.13.1 requires. New fields take `#[serde(default)]`. Tests in `config.rs` and `sync/state.rs` pin the 1.x shapes.

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

The `build` job (self-hosted macOS) signs and notarizes the macOS binaries, creates the tag and the release. The `linux` job then builds static musl binaries for x86_64 and aarch64 in a `rust:1` container and uploads them to the same release. The `homebrew` job waits for both and writes the formula from the release checksums. Each target ships as `tether-<target>.tar.gz` with a `.sha256` file. To move the Linux builds to a self-hosted runner, change the `runner` values in the `linux` matrix. The runner needs Docker.

**Do NOT create tags manually** - the release workflow handles tagging.

### Versioning

- **Patch** (1.0.x): Bug fixes
- **Minor** (1.x.0): New features, backward compatible
- **Major** (x.0.0): Breaking changes
- **Prerelease**: Use `-beta.N` suffix (creates versioned formula)

Users install via:
```bash
brew tap paddo-tech/tap
brew install tether-cli
```

Homebrew on Linux uses the same commands. Without Homebrew, Linux users download `tether-x86_64-unknown-linux-musl.tar.gz` or `tether-aarch64-unknown-linux-musl.tar.gz` from the GitHub release and put `tether` on `PATH`. The binaries are static and need no particular glibc.
