# Tether CLI

> Sync your development environment across multiple machines automatically.

[![GitHub Release](https://img.shields.io/github/v/release/paddo-tech/tether-cli)](https://github.com/paddo-tech/tether-cli/releases)
[![Build Status](https://img.shields.io/github/actions/workflow/status/paddo-tech/tether-cli/release.yml?branch=main)](https://github.com/paddo-tech/tether-cli/actions)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.91%2B-orange.svg)](https://www.rust-lang.org/)
[![macOS Intel](https://img.shields.io/badge/macOS-Intel-success)](https://github.com/paddo-tech/tether-cli/releases)
[![macOS Apple Silicon](https://img.shields.io/badge/macOS-Apple%20Silicon-success)](https://github.com/paddo-tech/tether-cli/releases)
[![Linux](https://img.shields.io/badge/Linux-Beta-yellow)](https://github.com/paddo-tech/tether-cli)

[![GitHub Stars](https://img.shields.io/github/stars/paddo-tech/tether-cli?style=social)](https://github.com/paddo-tech/tether-cli)
[![Buy Me a Coffee](https://img.shields.io/badge/Buy%20Me%20a%20Coffee-support-yellow?logo=buy-me-a-coffee)](https://buymeacoffee.com/paddotech)

**Website:** [tether-cli.com](https://tether-cli.com)

## What is Tether?

Tether syncs your shell configurations (`.zshrc`, `.gitconfig`, etc.) and globally installed packages across all your machines. Update a config or install a package on one machine, and it's available everywhere.

All dotfiles are **encrypted with AES-256-GCM** before syncing to Git. Your secrets stay secret.

### Key Features

- **End-to-end encryption** - Dotfiles encrypted with AES-256-GCM before syncing
- **Secret detection** - Automatic scanning for API keys, tokens, and credentials
- **Project configs** - Sync .env files and IDE settings by Git remote URL
- **Team sync** - Share dotfiles, secrets, and project configs with age public-key encryption
- **Collab secrets** - Collaborator-based project secret sharing using GitHub permissions
- **Package sync** - Homebrew, npm, pnpm, bun, gem, and uv
- **Background daemon** - Automatic periodic sync every 5 minutes
- **Git-backed** - Private Git repo for versioning and history

## Quick Start

```bash
# Install via Homebrew
brew tap paddo-tech/tap && brew install tether-cli

# Initialize (interactive setup)
tether init

# The daemon keeps everything in sync automatically
```

## Use Cases

### Multiple Machines
Laptop at work, desktop at home. Install a CLI tool on one machine, it's automatically on the other.

### New Machine Setup
Run `tether init` and all your dotfiles and packages are restored in minutes.

### Team Standardization
Share a sync repo across your team for consistent development environments, shared secrets, and project configs.

## What Gets Synced

### Dotfiles (Encrypted)
- `.zshrc`, `.gitconfig`, `.zprofile`, and custom dotfiles
- Stored encrypted as `.enc` files in Git, plaintext locally for your shell
- Encryption key derived from your passphrase using age encryption

### Packages (Plaintext)
- **Homebrew** - Formulae, casks, and taps
- **npm** / **pnpm** / **bun** - Global packages
- **gem** - Ruby gems
- **uv** - Python packages

## Commands

```bash
tether                   # Interactive dashboard
tether init              # Set up Tether on this machine
tether sync              # Manually trigger a sync
tether status            # Show current sync status
tether diff              # Show differences between machines
tether config            # Manage configuration and feature toggles
tether daemon            # Control the background daemon
tether machines          # Manage connected machines
tether ignore            # Manage ignore patterns
tether team              # Manage team sync (dotfiles, secrets, projects)
tether collab            # Collaborator-based project secret sharing
tether resolve           # Resolve file conflicts
tether unlock / lock     # Manage encryption key
tether upgrade           # Upgrade all installed packages
tether packages          # List and manage installed packages
tether restore           # Restore files from backup
tether identity          # Manage age identity for team secrets
```

## Security

### Encryption
All dotfiles are encrypted with **AES-256-GCM** (authenticated encryption) before being stored in Git. Fresh random nonce for each encryption, tamper detection built-in.

### Secret Detection
Scans for AWS keys, GitHub tokens, API keys, SSH private keys, passwords, database URLs, bearer tokens, and high-entropy strings before syncing.

### Key Management
Passphrase-based encryption. Set a passphrase on your first machine, enter the same passphrase on others. No cloud services or platform-specific keychains required.

### Privacy
- Encrypted at rest in Git, plaintext locally
- No external services -- data stays in your Git repo
- You own the repo and your passphrase

## How It Works

1. **Initialize** - Point Tether to your private Git repo; set a passphrase
2. **Scan** - Dotfiles scanned for secrets (API keys, tokens, etc.)
3. **Encrypt** - Dotfiles encrypted with AES-256-GCM
4. **Sync** - Encrypted dotfiles and package manifests pushed to Git
5. **Propagate** - Other machines pull, decrypt, and apply

## Architecture

Built with Rust for performance, reliability, and single-binary distribution.

```
+-----------------+
|   File Watcher  |  Monitors dotfiles for changes
+--------+--------+
         |
         v
+-----------------+
| Security Module |  Scans for secrets, encrypts with AES-256-GCM
+--------+--------+
         |
         v
+-----------------+
|   Sync Engine   |  Handles Git operations, conflict resolution
+--------+--------+
         |
         v
+-----------------+
|  Package Mgrs   |  brew, npm, pnpm, bun, gem, uv
+--------+--------+
         |
         v
+-----------------+
|   Git Backend   |  Your private repo (dotfiles encrypted)
+-----------------+
```

## Technology

- **Language:** Rust
- **CLI:** clap
- **Async:** tokio
- **Git:** git2
- **Encryption:** aes-gcm (AES-256-GCM), age (passphrase-based)
- **Secret Detection:** regex pattern matching

## FAQ

**Is my data secure?**
Yes. All dotfiles are encrypted with AES-256-GCM before Git storage. Keys are derived from your passphrase, never stored in Git. Even with repo access, dotfiles can't be decrypted without your passphrase.

**How does key sync work?**
Passphrase-based. Set a passphrase on your first machine, enter the same one on others. No cloud services required.

**Can I disable encryption?**
Set `encrypt_dotfiles = false` in `~/.tether/config.toml`. Not recommended if your dotfiles contain secrets.

**What about different packages on different machines?**
Use ignore patterns or machine-specific overrides.

**Does this work with multiple shells?**
Tether syncs any dotfile you configure. Default discovery targets zsh files, but you can add any shell's config files.

**Can I use this with my team?**
Yes. Team sync supports shared dotfiles, encrypted secrets with age, and project config sharing. Use `tether team setup` to get started.

**What happens offline?**
Changes are queued locally and synced when you're back online.

## Repository Structure

- **`/src`** - Tether CLI source code (Rust)
- **`/website`** - Marketing website (Astro.js) at [tether-cli.com](https://tether-cli.com)

## Documentation

- [CHANGELOG.md](CHANGELOG.md) - Version history

## License

MIT License - See [LICENSE](LICENSE) for details.

## Author

Built by [Paddo Tech](https://github.com/paddo-tech)

## Contributing

Contributions, issues, and feature requests are welcome. See the [issues page](https://github.com/paddo-tech/tether-cli/issues) to get started.
