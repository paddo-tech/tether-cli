# Tether CLI

> Sync your development environment across multiple machines automatically.

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.70%2B-orange.svg)](https://www.rust-lang.org/)

**Website:** [tether-cli.com](https://tether-cli.com)

## What is Tether?

Tether automatically syncs your shell configurations (`.zshrc`, `.gitconfig`, etc.) and globally installed packages across all your machines. Install a package or update your config on one machine, and it's immediately available everywhere.

**All your dotfiles are encrypted** before syncing to Git using AES-256-GCM encryption. Your secrets stay secret, even in your private Git repository.

### Key Features

- 🔐 **End-to-end encryption** - Dotfiles encrypted with AES-256-GCM before syncing
- 🔍 **Secret detection** - Automatic scanning for API keys, tokens, and credentials
- 🔑 **Passphrase-based encryption** - Derive keys from a passphrase you remember
- 📦 **Package manager support** - Syncs Homebrew (Brewfiles), npm, and pnpm global packages
- 🔄 **Automatic syncing** - Background daemon keeps everything in sync
- 🗂️ **Dotfile management** - Encrypted shell configs synced across machines
- 🌳 **Git-backed** - Uses private Git repo for versioning and history
- 🔒 **Privacy-focused** - Encrypted data in Git, keys derived from passphrase

## Quick Start

```bash
# Install via Homebrew
brew install tether-cli

# Initialize with your private sync repo
tether init --repo git@github.com:username/tether-sync.git

# That's it! The daemon will keep everything in sync automatically.
```

## Use Cases

### Scenario 1: Multiple Machines
You have a laptop for work and a desktop at home. Install a CLI tool on one machine, and it's automatically installed on the other.

### Scenario 2: New Machine Setup
Got a new machine? Run `tether init` and all your dotfiles and packages are restored in minutes.

### Scenario 3: Team Standardization
Share a sync repo across your team to maintain consistent development environments.

## What Gets Synced?

### Dotfiles (Encrypted)
- `.zshrc` (shell configuration) - **Encrypted with AES-256-GCM**
- `.gitconfig` (Git settings) - **Encrypted with AES-256-GCM**
- `.zprofile` (optional) - **Encrypted with AES-256-GCM**
- Custom dotfiles (configurable) - **All encrypted**

**How it works:**
- Local: Dotfiles stored as plaintext in `~/` (so your shell can read them)
- Git: Dotfiles stored encrypted as `.enc` files
- Encryption key: Derived from your passphrase using age encryption

### Packages (Plaintext)
- **Homebrew** - Synced via Brewfile (standard Homebrew format)
- **npm** - Global packages synced as simple text list
- **pnpm** - Global packages synced as simple text list
- More package managers coming soon (cargo, pipx, gem)

**Note:** Package manifests are not encrypted since package names are not sensitive.

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

## Security Features

### 🔐 Encryption
All dotfiles are encrypted using **AES-256-GCM** (authenticated encryption) before being stored in Git:
- **Algorithm:** AES-256-GCM (industry standard)
- **Key Size:** 256-bit encryption keys
- **Authenticated:** Tamper detection built-in
- **Unique:** Fresh random nonce for each encryption

### 🔍 Secret Detection
Tether automatically scans your dotfiles for sensitive data before syncing:
- **AWS keys** (access keys, secret keys)
- **GitHub tokens** (PATs, OAuth tokens)
- **API keys** (generic pattern matching)
- **Private keys** (SSH, RSA, OpenSSH)
- **Passwords** (plaintext passwords in configs)
- **Database URLs** (with embedded credentials)
- **High-entropy strings** (potential secrets)

When secrets are detected, Tether warns you and shows what was found (redacted). Your secrets are then safely encrypted before syncing.

### 🔑 Key Management
Encryption keys are derived from a **passphrase**:
- **First machine:** Enter a passphrase to generate your encryption key
- **Subsequent machines:** Enter the same passphrase to unlock
- **Portable:** Works on any machine - no platform-specific keychain needed
- **Secure:** Keys derived using age encryption, never stored in Git

### 🔒 Privacy
- **Encrypted at rest:** Dotfiles stored as `.enc` files in Git
- **Plaintext locally:** Your shell reads plaintext `~/.zshrc` normally
- **No external services:** Keys derived from your passphrase, data in your Git repo
- **Full control:** You own the Git repo and your passphrase

## How It Works

1. **Install** - `brew install tether-cli`
2. **Initialize** - Point Tether to your private Git repo; set a passphrase for encryption
3. **Scan** - Tether scans your dotfiles for secrets (API keys, tokens, etc.)
4. **Encrypt** - Dotfiles are encrypted with AES-256-GCM using key derived from passphrase
5. **Sync** - Encrypted dotfiles and package manifests pushed to Git
6. **Propagate** - Other machines pull encrypted data from Git
7. **Decrypt** - Other machines decrypt dotfiles using the same passphrase
8. **Apply** - Plaintext dotfiles written locally; packages installed automatically

## Architecture

Built with Rust for performance, reliability, and single-binary distribution.

```
┌─────────────────┐
│   File Watcher  │  Monitors dotfiles for changes
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│ Security Module │  Scans for secrets, encrypts with AES-256-GCM
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│   Sync Engine   │  Handles Git operations, conflict resolution
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│  Package Mgrs   │  Interfaces with brew (Brewfiles), npm, pnpm
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│   Git Backend   │  Your private repo (dotfiles encrypted)
└─────────────────┘
```

## Development Status

🚧 **Currently in active development**

### Roadmap

**Completed:**
- ✅ Core sync functionality (dotfiles + packages)
- ✅ Homebrew sync with Brewfiles
- ✅ npm/pnpm global package sync
- ✅ AES-256-GCM encryption for dotfiles
- ✅ Secret detection (API keys, tokens, etc.)
- ✅ Passphrase-based key management
- ✅ Git backend (GitHub, GitLab, self-hosted)
- ✅ Background daemon with launchd integration

**In Progress:**
- [ ] Enhanced conflict resolution
- [ ] Machine-specific overrides
- [ ] Rollback support

**Planned:**
- [ ] v1.2 - Additional package managers (cargo, pipx, gem)
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
- **Encryption:** aes-gcm (AES-256-GCM)
- **Key derivation:** age (passphrase-based encryption)
- **Secret Detection:** regex (pattern matching)
- **Randomness:** rand (cryptographic RNG)

## FAQ

**Q: Is my data secure?**
A: Yes. All dotfiles are encrypted with AES-256-GCM before being stored in Git. Encryption keys are derived from your passphrase (never stored in Git). Tether also scans for secrets (API keys, tokens) and warns you before syncing. Even if someone gains access to your Git repo, they cannot decrypt your dotfiles without your passphrase.

**Q: How does encryption key sync work?**
A: Tether uses passphrase-based encryption. On your first machine, you set a passphrase that derives your encryption key. On other machines, enter the same passphrase to unlock. No cloud services required.

**Q: What if I don't want encryption?**
A: You can disable it in `~/.tether/config.toml` by setting `encrypt_dotfiles = false`. However, we strongly recommend keeping encryption enabled, especially if your dotfiles contain API keys or tokens.

**Q: What secrets does Tether detect?**
A: AWS keys, GitHub tokens, API keys, SSH private keys, passwords, database URLs, bearer tokens, and high-entropy strings that might be secrets. When detected, Tether warns you before syncing.

**Q: What if I have different packages on different machines intentionally?**
A: Use ignore patterns or machine-specific overrides. Tether is flexible.

**Q: Does this work with multiple shells?**
A: v1.0 focuses on zsh. Support for bash, fish, and others is planned for v2.0.

**Q: Can I use this with my team?**
A: Yes! Share a sync repo to maintain consistent environments across your team. All team members use the same passphrase to access encrypted dotfiles.

**Q: What happens when I'm offline?**
A: Changes are queued locally and synced when you're back online. Nothing is lost.

## Repository Structure

This is a monorepo containing:
- **`/src`** - Tether CLI source code (Rust)
- **`/website`** - Marketing website (Astro.js) at [tether-cli.com](https://tether-cli.com)

See `/website/README.md` for website development instructions.

## License

MIT License - See [LICENSE](LICENSE) for details.

## Author

Built by [Paddo Tech](https://github.com/paddo-tech)

## Contributing

This project will be open-sourced once it reaches v1.0. Contributions, issues, and feature requests will be welcome!

---

**Follow development:** ⭐ Star this repo to get notified when we go public!
