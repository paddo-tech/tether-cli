# Tether CLI - Technical Specification

**Version:** 1.0.0
**Status:** Draft
**Domain:** tether-cli.com

## Overview

Tether CLI is a command-line tool for automatically syncing development environment configurations and globally installed packages across multiple Mac computers. When you update your `.zshrc` or install a global package on one machine, it automatically propagates to all your other machines.

## Project Metadata

- **Repository:** github.com/paddo-tech/tether-cli
- **License:** MIT (when open-sourced)
- **Language:** Rust
- **Distribution:** Homebrew, direct binary download
- **Platform:** macOS (v1), Linux support planned for v2

## Core Capabilities

### 1. Configuration Sync

**Supported Files:**
- `.zshrc` (primary)
- `.zprofile` (optional)
- `.gitconfig` (optional)
- Other dotfiles (configurable)

**Functionality:**
- File system watching for real-time change detection
- Intelligent diff generation and merge conflict resolution
- Selective sync (ignore sections via comments like `# tether:ignore`)
- Machine-specific overrides support
- Encrypted sync for sensitive configurations

**Technical Approach:**
- Use file hashing (SHA-256) to detect actual content changes
- Maintain `.tether/state.json` to track last known sync state per file
- Git-based versioning for full history and rollback capability

### 2. Package Manager Sync

**Supported Package Managers:**

#### Homebrew
- Sync installed formulae: `brew list --formula`
- Sync installed casks: `brew list --cask`
- Sync taps: `brew tap`
- Smart reconciliation: detect and install missing packages
- Optional: warn on packages present on one machine but not others

#### npm (global)
- Track: `npm list -g --depth=0 --json`
- Install missing: `npm install -g <package>`
- Version awareness: option to sync exact versions or allow latest

#### pnpm (global)
- Track: `pnpm list -g --depth=0 --json`
- Install missing: `pnpm add -g <package>`
- Version awareness: same as npm

**Future Package Managers (v2+):**
- cargo (Rust)
- pipx (Python)
- gem (Ruby)
- VS Code extensions

**Implementation Notes:**
- Store package manifests in `.tether/manifests/`
- Separate files: `brew.json`, `npm.json`, `pnpm.json`
- Track installation timestamps to handle conflicts
- Dry-run mode to preview package installations

### 3. Sync Mechanism

**Primary Backend: Git-based**
- Uses a private Git repository (GitHub, GitLab, self-hosted)
- Each machine commits changes with machine identifier
- Pull-merge-push workflow with conflict detection
- Supports SSH and HTTPS authentication

**File Structure in Sync Repo:**
```
tether-sync/
├── dotfiles/
│   ├── .zshrc
│   ├── .zprofile
│   └── .gitconfig
├── manifests/
│   ├── brew.json
│   ├── npm.json
│   └── pnpm.json
├── machines/
│   ├── machine-1.json
│   └── machine-2.json
└── .tether-meta
```

**Alternative Backends (Future):**
- iCloud Drive sync
- Dropbox sync
- Custom S3-compatible storage

**Sync Modes:**
1. **Daemon Mode:** Background process watches for changes and syncs automatically
2. **Manual Mode:** User runs `tether sync` when ready
3. **On-demand:** Sync triggered by package manager hooks

### 4. Conflict Resolution

**Strategy Options:**
- **Last-write-wins:** Default, timestamp-based
- **Manual merge:** Present diff and ask user to resolve
- **Machine-priority:** Designate a primary machine that always wins
- **Interactive:** CLI prompts for each conflict

**Conflict Detection:**
- Detect simultaneous edits on different machines
- Use three-way merge for dotfiles (base, theirs, ours)
- For packages: union of all installed packages (install everywhere)

## Command-Line Interface

### Installation
```bash
brew install tether-cli
```

### Commands

#### `tether init`
Interactive setup wizard:
- Choose sync backend (Git repo URL)
- Configure what to sync (dotfiles, package managers)
- Set sync frequency (daemon interval, manual-only, etc.)
- Generate SSH keys if needed
- Perform initial sync

**Flags:**
- `--backend <git|icloud|dropbox>` - Choose sync backend
- `--repo <url>` - Git repository URL
- `--no-daemon` - Don't start background daemon

#### `tether sync`
Manually trigger a sync operation:
1. Pull latest changes from sync backend
2. Detect local changes
3. Merge and resolve conflicts
4. Push updates
5. Install missing packages

**Flags:**
- `--dry-run` - Show what would be synced without doing it
- `--force` - Skip conflict prompts, use default strategy
- `--packages-only` - Only sync packages, not dotfiles
- `--dotfiles-only` - Only sync dotfiles, not packages

#### `tether status`
Show current sync status:
- Last sync timestamp per machine
- Files/packages out of sync
- Pending changes not yet synced
- Daemon status (running/stopped)

**Output Example:**
```
Tether Status
─────────────────────────────────────
Daemon:        Running (PID 12345)
Last Sync:     2 minutes ago
Sync Backend:  git@github.com:user/tether-sync.git

Files:
  ✓ .zshrc         synced
  ✓ .gitconfig     synced
  ⚠ .zprofile      modified locally (not synced)

Packages:
  ✓ Homebrew       37 formulae, 12 casks synced
  ⚠ npm            2 packages to install
  ✓ pnpm           synced

Machines:
  • macbook-pro-2021 (this machine)
  • macbook-air-2024 (last seen: 5 mins ago)
```

#### `tether diff`
Show differences between this machine and others:
- File diffs for dotfiles
- Package differences (installed here vs other machines)

**Flags:**
- `--machine <name>` - Compare with specific machine
- `--packages` - Only show package differences
- `--files` - Only show file differences

#### `tether daemon`
Control the background daemon:
- `tether daemon start` - Start daemon
- `tether daemon stop` - Stop daemon
- `tether daemon restart` - Restart daemon
- `tether daemon logs` - View daemon logs
- `tether daemon install` - Install launchd service (auto-start on login, auto-restart)
- `tether daemon uninstall` - Remove launchd service

The daemon automatically detects when the tether binary is updated (e.g., via `brew upgrade`) and exits gracefully. When installed as a launchd service, it will automatically restart with the new version.

#### `tether rollback`
Rollback to a previous sync state:
- `tether rollback` - Interactive selection
- `tether rollback --commit <sha>` - Rollback to specific Git commit
- `tether rollback --preview` - Show what would be restored

#### `tether machines`
Manage machines in the sync network:
- `tether machines list` - List all machines
- `tether machines rename <old> <new>` - Rename this machine
- `tether machines remove <name>` - Remove a machine from sync

#### `tether ignore`
Manage ignore patterns:
- `tether ignore add <pattern>` - Add ignore pattern
- `tether ignore list` - Show all ignore patterns
- `tether ignore remove <pattern>` - Remove ignore pattern

**Ignore patterns:**
- File paths: `~/.aws/credentials`
- Package names: `npm:local-test-package`
- Entire categories: `brew:casks`

#### `tether config`
Manage configuration:
- `tether config get <key>` - Get config value
- `tether config set <key> <value>` - Set config value
- `tether config edit` - Open config in $EDITOR

**Key Configuration Options:**
- `sync.interval` - Daemon sync interval (default: 5m)
- `sync.strategy` - Conflict resolution strategy
- `backend.type` - Sync backend type
- `backend.url` - Git repository URL
- `packages.npm.enabled` - Enable npm sync
- `packages.pnpm.enabled` - Enable pnpm sync
- `packages.brew.enabled` - Enable Homebrew sync

## Technical Architecture

### Technology Stack

**Language:** Rust

**Rationale:**
- Single binary distribution (no runtime dependencies)
- Excellent performance for file watching and daemon processes
- Strong ecosystem for CLI tools
- Memory safe, ideal for long-running daemon
- Cross-platform support for future Linux/Windows ports

**Key Dependencies:**

| Crate | Purpose |
|-------|---------|
| `clap` | Command-line argument parsing and CLI framework |
| `tokio` | Async runtime for concurrent operations |
| `notify` | File system event watching |
| `serde` + `serde_json` | Configuration and manifest serialization |
| `git2` | Git operations (clone, commit, push, pull) |
| `toml` | Configuration file format |
| `sha2` | File hashing for change detection |
| `chrono` | Timestamp handling |
| `anyhow` | Error handling |
| `dialoguer` | Interactive CLI prompts |
| `indicatif` | Progress bars and spinners |
| `colored` | Terminal output coloring |

### Core Components

#### 1. File Watcher (`src/watcher.rs`)
- Uses `notify` crate to watch dotfiles for changes
- Debounces rapid changes (e.g., during large edits)
- Filters out temporary files (`.swp`, `.tmp`, etc.)
- Triggers sync on stable changes (no edits for 2 seconds)

#### 2. Sync Engine (`src/sync/`)
- `engine.rs` - Core sync orchestration
- `git.rs` - Git backend implementation
- `conflict.rs` - Conflict detection and resolution
- `state.rs` - State management (`.tether/state.json`)

#### 3. Package Manager Integration (`src/packages/`)
- `brew.rs` - Homebrew operations
- `npm.rs` - npm global package operations
- `pnpm.rs` - pnpm global package operations
- `manager.rs` - Generic package manager trait

#### 4. Daemon (`src/daemon/`)
- `server.rs` - Background daemon process
- `ipc.rs` - Inter-process communication (CLI ↔ daemon)
- Uses Unix domain sockets for IPC
- Daemon runs as launchd service on macOS

#### 5. CLI (`src/cli/`)
- `commands/` - Command implementations
- `output.rs` - Formatted output and UI
- `prompts.rs` - Interactive prompts

### File System Layout

**User's Home Directory:**
```
~/.tether/
├── config.toml          # User configuration
├── state.json           # Sync state tracking
├── daemon.log           # Daemon logs
├── daemon.pid           # Daemon process ID
├── sync/                # Local clone of sync repo
│   ├── dotfiles/
│   ├── manifests/
│   └── machines/
└── ignore               # Ignore patterns
```

**Config File Example (`~/.tether/config.toml`):**
```toml
[sync]
interval = "5m"
strategy = "last-write-wins"

[backend]
type = "git"
url = "git@github.com:username/tether-sync.git"

[packages.brew]
enabled = true
sync_casks = true
sync_taps = true

[packages.npm]
enabled = true
sync_versions = false  # false = install latest, true = exact versions

[packages.pnpm]
enabled = true
sync_versions = false

[dotfiles]
files = [".zshrc", ".zprofile", ".gitconfig"]
```

### State File Example (`~/.tether/state.json`)
```json
{
  "machine_id": "macbook-pro-2021",
  "last_sync": "2025-01-15T10:30:00Z",
  "files": {
    ".zshrc": {
      "hash": "a3f5e1b2c...",
      "last_modified": "2025-01-15T10:25:00Z",
      "synced": true
    },
    ".gitconfig": {
      "hash": "d4e6f2c3a...",
      "last_modified": "2025-01-14T15:20:00Z",
      "synced": true
    }
  },
  "packages": {
    "brew": {
      "last_sync": "2025-01-15T10:30:00Z",
      "hash": "b5c7d3e1f..."
    },
    "npm": {
      "last_sync": "2025-01-15T10:30:00Z",
      "hash": "c6d8e4f2a..."
    }
  }
}
```

## Security Considerations

### Sensitive Data Handling
- **SSH Keys:** Never sync SSH private keys
- **Credentials:** Support `.gitignore`-style patterns for sensitive files
- **Tokens:** Warn if API tokens detected in dotfiles
- **Encryption:** Option to encrypt dotfiles before syncing (using age or GPG)

### Authentication
- Git SSH keys (recommended)
- Git HTTPS with credential helper
- Personal Access Tokens (PAT) for GitHub

### Privacy
- All repos should be private
- Option to use self-hosted Git server
- No telemetry or analytics collected

## Performance Targets

- **Daemon CPU usage:** < 0.5% idle, < 5% during sync
- **Memory usage:** < 20MB idle, < 50MB during sync
- **Sync latency:** < 10 seconds for dotfile changes
- **Package sync:** < 2 minutes for full package installation
- **Binary size:** < 10MB

## Error Handling

### Network Failures
- Queue changes locally when offline
- Retry with exponential backoff
- Resume sync when connection restored

### Git Conflicts
- Detect merge conflicts in dotfiles
- Present diff to user with options:
  - Keep local
  - Keep remote
  - Manual merge
  - Abort

### Package Installation Failures
- Continue with other packages if one fails
- Log failures to `~/.tether/daemon.log`
- Retry failed installations on next sync
- Show summary of failures in `tether status`

## Testing Strategy

### Unit Tests
- Package manager parsers
- State management
- Hash generation
- Config validation

### Integration Tests
- End-to-end sync workflow
- Git operations
- Conflict resolution
- Package installation (mock)

### Manual Testing Scenarios
1. Fresh init on new machine
2. Simultaneous edits on two machines
3. Network interruption during sync
4. Package installation failure
5. Large dotfile changes (performance)

## Deployment

### Homebrew Distribution
```ruby
# Formula: tether-cli.rb
class TetherCli < Formula
  desc "Sync development environment across multiple Macs"
  homepage "https://tether-cli.com"
  url "https://github.com/paddo-tech/tether-cli/archive/v1.0.0.tar.gz"
  sha256 "..."
  license "MIT"

  depends_on :macos

  def install
    system "cargo", "build", "--release"
    bin.install "target/release/tether"
  end

  def post_install
    puts "Run 'tether init' to get started!"
  end
end
```

### Release Process
1. Tag release: `git tag v1.0.0`
2. Build binary: `cargo build --release`
3. Create GitHub release with binary artifacts
4. Update Homebrew formula
5. Publish to homebrew-core (after stable release)

### Installation Methods
```bash
# Homebrew (recommended)
brew install tether-cli

# Direct download
curl -fsSL https://tether-cli.com/install.sh | bash

# Cargo (for Rust users)
cargo install tether-cli

# Build from source
git clone https://github.com/paddo-tech/tether-cli
cd tether-cli
cargo build --release
```

## Roadmap

### v1.0 - MVP (3-4 months)
- ✅ Core sync engine (Git backend)
- ✅ Dotfile sync (.zshrc, .gitconfig)
- ✅ Homebrew sync (formulae + casks)
- ✅ npm global package sync
- ✅ pnpm global package sync
- ✅ Daemon mode
- ✅ Basic conflict resolution (last-write-wins)
- ✅ CLI commands: init, sync, status, diff
- ✅ Homebrew distribution

### v1.1 - Polish (1-2 months)
- Interactive conflict resolution
- Machine-specific overrides
- Rollback support
- Ignore patterns
- Encrypted dotfile sync
- Comprehensive error messages

### v1.2 - Extended Package Managers (1-2 months)
- cargo (Rust packages)
- pipx (Python tools)
- gem (Ruby gems)
- VS Code extensions sync

### v2.0 - Multi-Platform (3-4 months)
- Linux support (Ubuntu, Fedora, Arch)
- Support for bash, fish shells
- Alternative sync backends (iCloud, Dropbox)
- Web dashboard for viewing sync status

### v2.1 - Advanced Features
- Conditional sync rules (only sync on certain machines)
- Scheduled sync (cron-like)
- Sync profiles (work, personal, etc.)
- Team sync (share dotfiles across team members)

## Success Metrics

### User Metrics
- Time saved per week per user (target: 30+ minutes)
- Number of machines per user (target: 2-5)
- Sync success rate (target: 99%+)
- User retention after 30 days (target: 80%+)

### Technical Metrics
- Sync reliability (target: 99.9%)
- Average sync latency (target: < 10s)
- Daemon crash rate (target: < 0.1%)
- Package installation success rate (target: 95%+)

## Open Source Strategy

### Initial Phase (Private)
- Build MVP
- Test with small group of users
- Fix major bugs
- Write comprehensive documentation

### Public Release
- Finalize documentation
- Create contributing guidelines
- Set up issue templates
- Write code of conduct
- Prepare announcement (blog post, HN, Reddit)
- Make repository public
- Tag v1.0.0 release

### Community Growth
- Accept pull requests
- Respond to issues within 48 hours
- Monthly release cycle
- Maintain changelog
- Build contributor community

## Competitors & Differentiation

### Existing Solutions

**mackup:**
- Python-based
- 500+ application configs supported
- One-way sync (backup/restore, not continuous)
- Our advantage: Real-time sync, package managers, Rust performance

**dotfiles repos:**
- Manual Git management
- No automatic sync
- Our advantage: Automatic, handles packages too

**Syncthing:**
- General file sync
- Not dev-environment specific
- Our advantage: Smart package installation, conflict resolution

**Chezmoi:**
- Template-based dotfile manager
- Complex setup
- Our advantage: Zero-config automatic sync

### Our Unique Value Proposition
1. **Automatic sync** - Set it and forget it
2. **Package manager support** - Not just dotfiles
3. **Mac-native** - Optimized for macOS workflow
4. **Zero-config** - Works out of the box
5. **Open source** - Community-driven, transparent

## Documentation Plan

### User Documentation
- **README.md** - Quick start, installation
- **GETTING_STARTED.md** - Tutorial for first-time users
- **COMMANDS.md** - Comprehensive command reference
- **CONFIGURATION.md** - Config file documentation
- **TROUBLESHOOTING.md** - Common issues and solutions

### Developer Documentation
- **CONTRIBUTING.md** - How to contribute
- **ARCHITECTURE.md** - System design overview
- **API.md** - Internal API documentation (rustdoc)
- **CHANGELOG.md** - Version history

### Website (tether-cli.com)
- Landing page with demo
- Documentation hub
- Blog for announcements
- GitHub link prominent

## Questions & Decisions Needed

1. **Homebrew cask sync behavior:**
   - Install missing casks automatically, or prompt first?
   - Handle large apps (Xcode, etc.) differently?

2. **Version conflicts:**
   - If Machine A has `ripgrep@13.0.0` and Machine B has `ripgrep@14.0.0`, what do?
   - Options: Always upgrade to latest, keep both, prompt user

3. **Initial sync:**
   - On first `tether init`, should it push current state or pull from repo?
   - Probably: Prompt user to choose

4. **Daemon permissions:**
   - Should daemon run as user or require elevated privileges?
   - Probably: User-level, use sudo only when needed for brew installs

5. **Conflict resolution UI:**
   - Terminal-based diff viewer, or open in external tool (vimdiff, meld)?
   - Both? Make it configurable?

## Appendix

### Example User Workflow

**Day 1 - Machine 1 (MacBook Pro):**
```bash
# Install tether
brew install tether-cli

# Initialize with GitHub repo
tether init --repo git@github.com:username/tether-sync.git

# Daemon starts automatically
# Current .zshrc and packages are synced to repo
```

**Day 1 - Machine 2 (MacBook Air):**
```bash
# Install tether
brew install tether-cli

# Initialize (same repo)
tether init --repo git@github.com:username/tether-sync.git

# Tether detects existing sync data
# Prompts: "Found existing sync. Pull changes? (Y/n)"
# User confirms
# Packages are installed, .zshrc is updated
```

**Day 5 - Machine 1:**
```bash
# User installs new package
brew install ripgrep

# Daemon detects brew database change
# Syncs to repo within 10 seconds
```

**Day 5 - Machine 2 (5 minutes later):**
```bash
# Daemon pulls update
# Detects new package: ripgrep
# Runs: brew install ripgrep
# Notifies user: "Installed 1 new package: ripgrep"
```

**Day 10 - Machine 1:**
```bash
# User edits .zshrc, adds alias
echo 'alias ll="ls -la"' >> ~/.zshrc

# Daemon detects file change
# Syncs within 2 seconds
```

**Day 10 - Machine 2:**
```bash
# Daemon pulls update
# Updates .zshrc
# User's next shell has the new alias
```

### Technical Challenges & Solutions

**Challenge 1: Race conditions when both machines edit simultaneously**

Solution:
- Git's merge system handles this
- If conflicts: use three-way merge
- Last-write-wins by default, but preserve both in conflict markers
- User resolves on next `tether sync`

**Challenge 2: Large packages slowing down sync**

Solution:
- Package installation happens async, non-blocking
- User gets notification when complete
- Can configure auto-install vs. prompt

**Challenge 3: Sensitive data in dotfiles**

Solution:
- Built-in patterns ignore common sensitive files (.env, .aws/credentials)
- Users can add custom ignore patterns
- Option to encrypt entire sync repo with GPG/age

**Challenge 4: Network unreliable (coffee shop, airplane)**

Solution:
- Queue changes locally in `.tether/queue/`
- Retry with exponential backoff
- Sync when network returns
- Never lose data

---

**Document Version:** 1.0
**Last Updated:** 2025-01-15
**Author:** Paddo Tech
**Status:** Draft - Ready for Development
