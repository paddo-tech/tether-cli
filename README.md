# Tether CLI

> Sync your development environment across multiple Macs automatically.

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.70%2B-orange.svg)](https://www.rust-lang.org/)

**Website:** [tether-cli.com](https://tether-cli.com)

## What is Tether?

Tether automatically syncs your shell configurations (`.zshrc`, `.gitconfig`, etc.) and globally installed packages across all your Mac computers. Install a package or update your config on one machine, and it's immediately available everywhere.

**All your dotfiles are encrypted** before syncing to Git using AES-256-GCM encryption. Your secrets stay secret, even in your private Git repository.

### Key Features

- 🔐 **End-to-end encryption** - Dotfiles encrypted with AES-256-GCM before syncing
- 🔍 **Secret detection** - Automatic scanning for API keys, tokens, and credentials
- 🔑 **iCloud Keychain** - Encryption keys sync automatically across your Macs
- 📦 **Package manager support** - Syncs Homebrew (Brewfiles), npm, and pnpm global packages
- 🔄 **Automatic syncing** - Background daemon keeps everything in sync (coming soon)
- 🗂️ **Dotfile management** - Encrypted shell configs synced across machines
- 🌳 **Git-backed** - Uses private Git repo for versioning and history
- 🔒 **Privacy-focused** - Encrypted data in Git, keys in iCloud Keychain

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

### Dotfiles (Encrypted)
- `.zshrc` (shell configuration) - **Encrypted with AES-256-GCM**
- `.gitconfig` (Git settings) - **Encrypted with AES-256-GCM**
- `.zprofile` (optional) - **Encrypted with AES-256-GCM**
- Custom dotfiles (configurable) - **All encrypted**

**How it works:**
- Local: Dotfiles stored as plaintext in `~/` (so your shell can read them)
- Git: Dotfiles stored encrypted as `.enc` files
- Encryption key: Stored securely in iCloud Keychain, syncs across your Macs

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
Encryption keys are stored in **iCloud Keychain**:
- **First machine:** Generates a new encryption key automatically
- **Subsequent machines:** Finds the key already synced via iCloud
- **Zero configuration:** Works transparently across all your Macs
- **Secure:** Keys never stored in Git, only in Apple's encrypted Keychain

### 🔒 Privacy
- **Encrypted at rest:** Dotfiles stored as `.enc` files in Git
- **Plaintext locally:** Your shell reads plaintext `~/.zshrc` normally
- **No external services:** Keys in iCloud Keychain, data in your Git repo
- **Full control:** You own the Git repo and the Keychain data

## How It Works

1. **Install** - `brew install tether-cli`
2. **Initialize** - Point Tether to your private Git repo; encryption key generated and stored in iCloud Keychain
3. **Scan** - Tether scans your dotfiles for secrets (API keys, tokens, etc.)
4. **Encrypt** - Dotfiles are encrypted with AES-256-GCM using key from Keychain
5. **Sync** - Encrypted dotfiles and package manifests pushed to Git
6. **Propagate** - Other machines pull encrypted data from Git
7. **Decrypt** - Other machines decrypt dotfiles using synced key from iCloud Keychain
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
         │
         ▼
┌─────────────────┐
│ iCloud Keychain │  Encryption keys (auto-syncs across Macs)
└─────────────────┘
```

## Development Status

🚧 **Currently in active development** - This repository is private while we build the MVP.

### Roadmap

**Completed:**
- ✅ Core sync functionality (dotfiles + packages)
- ✅ Homebrew sync with Brewfiles
- ✅ npm/pnpm global package sync
- ✅ AES-256-GCM encryption for dotfiles
- ✅ Secret detection (API keys, tokens, etc.)
- ✅ iCloud Keychain integration for key management
- ✅ Git backend (GitHub, GitLab, self-hosted)

**In Progress:**
- [ ] Background daemon for automatic syncing
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
- **Keychain:** security-framework (macOS Keychain/iCloud)
- **Secret Detection:** regex (pattern matching)
- **Randomness:** rand (cryptographic RNG)

## FAQ

**Q: Is my data secure?**
A: Yes. All dotfiles are encrypted with AES-256-GCM before being stored in Git. Encryption keys are stored in iCloud Keychain (never in Git). Tether also scans for secrets (API keys, tokens) and warns you before syncing. Even if someone gains access to your Git repo, they cannot decrypt your dotfiles without the encryption key from your iCloud Keychain.

**Q: How does encryption key sync work?**
A: On your first machine, Tether generates a 256-bit encryption key and stores it in iCloud Keychain. When you initialize Tether on another Mac with the same iCloud account, the key is automatically available (synced by iCloud). No manual key management needed.

**Q: What if I don't want encryption?**
A: You can disable it in `~/.tether/config.toml` by setting `encrypt_dotfiles = false`. However, we strongly recommend keeping encryption enabled, especially if your dotfiles contain API keys or tokens.

**Q: What secrets does Tether detect?**
A: AWS keys, GitHub tokens, API keys, SSH private keys, passwords, database URLs, bearer tokens, and high-entropy strings that might be secrets. When detected, Tether warns you before syncing.

**Q: What if I have different packages on different machines intentionally?**
A: Use ignore patterns or machine-specific overrides. Tether is flexible.

**Q: Does this work with multiple shells?**
A: v1.0 focuses on zsh. Support for bash, fish, and others is planned for v2.0.

**Q: Can I use this with my team?**
A: Yes! Share a sync repo to maintain consistent environments across your team. Note that all team members would need to share the same encryption key (stored in a shared iCloud account or distributed manually).

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
