# Tether CLI

> Sync your development environment across multiple Macs automatically.

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.70%2B-orange.svg)](https://www.rust-lang.org/)

**Website:** [tether-cli.com](https://tether-cli.com)

## What is Tether?

Tether automatically syncs your shell configurations (`.zshrc`, `.gitconfig`, etc.) and globally installed packages across all your Mac computers. Install a package or update your config on one machine, and it's immediately available everywhere.

### Key Features

- **Automatic syncing** - Changes propagate automatically via background daemon
- **Package manager support** - Syncs Homebrew, npm, and pnpm global packages
- **Dotfile management** - Keeps shell configs in sync across machines
- **Conflict resolution** - Smart merge strategies with manual override options
- **Git-backed** - Uses private Git repo for versioning and history
- **Privacy-focused** - Your data stays in your private repo, no external services

## Quick Start

```bash
# Install via Homebrew
brew install tether-cli

# Initialize with your private sync repo
tether init --repo git@github.com:username/tether-sync.git

# That's it! The daemon will keep everything in sync automatically.
```

## Use Cases

### Scenario 1: Multiple Macs
You have a MacBook Pro for work and a MacBook Air for personal use. Install a CLI tool on one machine, and it's automatically installed on the other.

### Scenario 2: New Machine Setup
Got a new Mac? Run `tether init` and all your dotfiles and packages are restored in minutes.

### Scenario 3: Team Standardization
Share a sync repo across your team to maintain consistent development environments.

## What Gets Synced?

### Dotfiles
- `.zshrc` (shell configuration)
- `.gitconfig` (Git settings)
- `.zprofile` (optional)
- Custom dotfiles (configurable)

### Packages
- **Homebrew** formulae and casks
- **npm** global packages
- **pnpm** global packages
- More package managers coming soon (cargo, pipx, gem)

## Commands

```bash
tether init          # Set up Tether on this machine
tether sync          # Manually trigger a sync
tether status        # Show current sync status
tether diff          # Show differences between machines
tether daemon        # Control background daemon
tether machines      # Manage connected machines
tether rollback      # Revert to previous state
```

See [SPEC.md](SPEC.md) for comprehensive documentation.

## How It Works

1. **Install** - `brew install tether-cli`
2. **Initialize** - Point Tether to your private Git repo
3. **Sync** - Tether creates a snapshot of your dotfiles and installed packages
4. **Watch** - Background daemon monitors for changes
5. **Propagate** - Changes are pushed to Git and pulled by other machines
6. **Apply** - Other machines install new packages and update configs automatically

## Architecture

Built with Rust for performance, reliability, and single-binary distribution.

```
┌─────────────────┐
│   File Watcher  │  Monitors dotfiles for changes
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│   Sync Engine   │  Handles Git operations, conflict resolution
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│  Package Mgrs   │  Interfaces with brew, npm, pnpm
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│   Git Backend   │  Your private GitHub/GitLab repo
└─────────────────┘
```

## Development Status

🚧 **Currently in active development** - This repository is private while we build the MVP.

### Roadmap

- [ ] v1.0 - MVP with core sync functionality
- [ ] v1.1 - Enhanced conflict resolution
- [ ] v1.2 - Additional package managers
- [ ] v2.0 - Linux support
- [ ] Public release

See [SPEC.md](SPEC.md) for detailed roadmap.

## Documentation

- [SPEC.md](SPEC.md) - Complete technical specification
- [CONTRIBUTING.md](CONTRIBUTING.md) - Contribution guidelines *(coming soon)*
- [CHANGELOG.md](CHANGELOG.md) - Version history *(coming soon)*

## Technology

- **Language:** Rust
- **CLI Framework:** clap
- **Async Runtime:** tokio
- **File Watching:** notify
- **Git Operations:** git2

## FAQ

**Q: Is my data secure?**
A: Yes. Tether uses your own private Git repository. Your dotfiles and package lists never leave your control.

**Q: What if I have different packages on different machines intentionally?**
A: Use ignore patterns or machine-specific overrides. Tether is flexible.

**Q: Does this work with multiple shells?**
A: v1.0 focuses on zsh. Support for bash, fish, and others is planned for v2.0.

**Q: Can I use this with my team?**
A: Yes! Share a sync repo to maintain consistent environments across your team.

**Q: What happens when I'm offline?**
A: Changes are queued locally and synced when you're back online. Nothing is lost.

## License

MIT License - See [LICENSE](LICENSE) for details.

## Author

Built by [Paddo Tech](https://github.com/paddo-tech)

## Contributing

This project will be open-sourced once it reaches v1.0. Contributions, issues, and feature requests will be welcome!

---

**Follow development:** ⭐ Star this repo to get notified when we go public!
