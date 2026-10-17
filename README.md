# Tether CLI

> Sync your dotfiles and global packages across your machines through your own Git repository.

[![GitHub Release](https://img.shields.io/github/v/release/paddo-tech/tether-cli)](https://github.com/paddo-tech/tether-cli/releases)
[![Build Status](https://img.shields.io/github/actions/workflow/status/paddo-tech/tether-cli/release.yml?branch=main)](https://github.com/paddo-tech/tether-cli/actions)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

**Website and docs:** [tether-cli.com](https://tether-cli.com)

## What is Tether?

Tether syncs your shell configs, such as `.zshrc` and `.gitconfig`, and your global Homebrew, npm, pnpm, bun, gem and uv packages. It encrypts dotfiles with AES-256-GCM before they reach Git. A background daemon syncs every 5 minutes.

Each machine signs a record of the packages it installed. A synced package installs on another machine only when that machine trusts the signing machine, and the package passes the checks: a release-age limit and an OSV malware check for npm, pnpm, bun, uv and gem, and a tap allowlist for Homebrew. Other packages for the machine's profile wait in an Inbox. You can approve them, unless OSV lists them as malware.

## Install

macOS and Linux, with Homebrew:

```bash
brew tap paddo-tech/tap && brew install tether-cli
```

Or with the install script, on macOS and Linux:

```bash
curl -fsSL https://tether-cli.com/install.sh | sh
```

Linux without Homebrew or the script: download `tether-x86_64-unknown-linux-musl.tar.gz` or `tether-aarch64-unknown-linux-musl.tar.gz` and its `.sha256` file from the [latest release](https://github.com/paddo-tech/tether-cli/releases/latest). See [Install](https://tether-cli.com/docs/install).

## Quick start

```bash
tether init                                          # first machine: pick the repo, set a passphrase
tether init --repo git@github.com:you/dotfiles.git   # each other machine: same repo, same passphrase
tether machines show                                 # on each machine: read its id and fingerprint
tether machines trust <id> --fingerprint SHA256:...  # on the other machines: trust it
tether sync && tether packages inbox                 # install trusted packages, review the rest
```

## Documentation

- [Quick start](https://tether-cli.com/docs/quick-start)
- [Upgrade from 1.x](https://tether-cli.com/docs/upgrade): read this before you upgrade a 1.x machine
- [Concepts](https://tether-cli.com/docs/how-it-works): sync, encryption, machine trust, the Inbox, profiles
- [Commands](https://tether-cli.com/docs/commands) and [Configuration](https://tether-cli.com/docs/configuration)
- [Troubleshooting](https://tether-cli.com/docs/troubleshooting)
- [Supply-chain security](https://tether-cli.com/supply-chain) and [Security](https://tether-cli.com/security)
- [CHANGELOG.md](CHANGELOG.md)

## License

MIT. See [LICENSE](LICENSE).

## Contributing

Issues and pull requests are welcome. See the [issues page](https://github.com/paddo-tech/tether-cli/issues).
