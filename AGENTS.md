# AGENTS.md

> **Note:** `CLAUDE.md` is symlinked to this file for compatibility with Claude Code.

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Tether CLI is a Rust-based command-line tool that automatically syncs development environment configurations (dotfiles) and globally installed packages across multiple Mac computers. It uses a Git-backed sync mechanism with a background daemon for real-time synchronization.

## Build and Development Commands

Since this is a Rust project, use standard Cargo commands:

```bash
# Build the project
cargo build

# Build optimized release binary
cargo build --release

# Run the CLI (development)
cargo run -- <command> [args]

# Run tests
cargo test

# Run tests with output
cargo test -- --nocapture

# Check code without building
cargo check

# Format code
cargo fmt

# Run linter
cargo clippy

# Build and install locally
cargo install --path .
```

## Architecture

### Core Components (from SPEC.md)

The codebase follows a modular architecture organized into these key modules:

**File Watcher** (`src/watcher.rs`)
- Uses the `notify` crate to monitor dotfile changes
- Debounces rapid changes and filters temporary files
- Triggers sync on stable changes (no edits for 2 seconds)

**Sync Engine** (`src/sync/`)
- `engine.rs` - Core sync orchestration logic
- `git.rs` - Git backend implementation (clone, commit, push, pull)
- `conflict.rs` - Conflict detection and resolution strategies
- `state.rs` - State management via `.tether/state.json`

**Package Manager Integration** (`src/packages/`)
- `brew.rs` - Homebrew operations (formulae, casks, taps)
- `npm.rs` - npm global package operations
- `pnpm.rs` - pnpm global package operations
- `manager.rs` - Generic package manager trait/interface

**Daemon** (`src/daemon/`)
- `server.rs` - Background daemon process
- `ipc.rs` - Inter-process communication between CLI and daemon
- Uses Unix domain sockets for IPC
- Runs as launchd service on macOS

**CLI** (`src/cli/`)
- `commands/` - Individual command implementations
- `output.rs` - Formatted terminal output and UI
- `prompts.rs` - Interactive CLI prompts

### Key Technology Stack

- **clap** - CLI framework and argument parsing
- **tokio** - Async runtime for concurrent operations
- **notify** - File system event watching
- **serde/serde_json** - Serialization for config and manifests
- **git2** - Git operations
- **sha2** - File hashing for change detection
- **anyhow** - Error handling
- **dialoguer** - Interactive prompts
- **indicatif** - Progress bars and spinners
- **colored** - Terminal output coloring

### File System Layout

User configuration lives in `~/.tether/`:
- `config.toml` - User configuration
- `state.json` - Sync state tracking (file hashes, timestamps)
- `daemon.log` - Daemon logs
- `daemon.pid` - Daemon process ID
- `sync/` - Local clone of the sync Git repository
- `ignore` - Ignore patterns for files/packages

The sync Git repository structure:
- `dotfiles/` - Synced dotfiles (.zshrc, .gitconfig, etc.)
- `manifests/` - Package manifests (brew.json, npm.json, pnpm.json)
- `machines/` - Machine-specific metadata

## Commands Overview

The CLI implements these primary commands:

- `tether init` - Interactive setup wizard, initializes sync
- `tether sync` - Manually trigger sync operation
- `tether status` - Show current sync status and health
- `tether diff` - Show differences between machines
- `tether daemon` - Control background daemon (start/stop/restart/logs)
- `tether rollback` - Rollback to previous sync state
- `tether machines` - Manage machines in sync network
- `tether ignore` - Manage ignore patterns
- `tether config` - Manage configuration settings

## Development Workflow

1. **Adding a new command**: Create implementation in `src/cli/commands/`, add to CLI parser
2. **Adding package manager support**: Implement the trait in `src/packages/manager.rs`, create new module
3. **Modifying sync logic**: Core sync engine is in `src/sync/engine.rs`
4. **Daemon changes**: Background process logic in `src/daemon/server.rs`

## Code Quality Standards

**CRITICAL**: Before declaring any work finished, ALWAYS ensure code passes linting checks:

```bash
# Run clippy with all warnings as errors (required before commits)
cargo clippy --all-targets --all-features -- -D warnings

# Format all code (required before commits)
cargo fmt --all

# Run full test suite
cargo test
```

**Requirements:**
- Zero clippy warnings allowed in main branch
- All code must be formatted with `cargo fmt`
- Prefer implementing standard traits (Default, Display, etc.) over custom methods
- Use `_` prefix for intentionally unused struct fields
- Remove all unused imports and dead code

**Before completing any task:**
1. Run `cargo clippy --all-targets --all-features -- -D warnings`
2. Fix ALL warnings (no exceptions)
3. Run `cargo fmt --all`
4. Verify with `cargo build --release`
5. Only then mark work as complete

## Important Notes

- This is a **macOS-only** tool in v1.0 (Linux support planned for v2.0)
- The project is currently **private** and in active development
- Designed for single-binary distribution (no runtime dependencies)
- Security-conscious: never sync SSH private keys, API tokens, or credentials
- Uses Git as the sync backend (GitHub, GitLab, or self-hosted)
- Daemon should maintain <0.5% CPU idle, <20MB memory usage

## Testing

Run full test suite with:
```bash
cargo test
```

Test categories:
- Unit tests: Package manager parsers, state management, hash generation
- Integration tests: End-to-end sync workflow, Git operations, conflict resolution
- Manual testing scenarios documented in SPEC.md section "Testing Strategy"

## Performance Targets

- Daemon CPU usage: <0.5% idle, <5% during sync
- Memory usage: <20MB idle, <50MB during sync
- Sync latency: <10 seconds for dotfile changes
- Binary size: <10MB
