# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [1.1.6] - 2026-01-04

### Fixed

- Explicitly tap missing brew taps before bundle install (fixes formulae from taps not found)

## [1.1.5] - 2026-01-04

### Fixed

- SSH passphrase prompts now work during git operations (fixes #1)

## [1.1.4] - 2026-01-03

### Changed

- Casks now install individually instead of being blanket-skipped in daemon mode
- Only casks that actually require password are flagged for manual sync
- Notifications only trigger once per unique deferred cask list (no repeated alerts)

## [1.1.3] - 2025-12-30

### Fixed

- Package upgrades now catch up after sleep (was skipped if Mac asleep at 2am)

## [1.1.2] - 2025-12-22

### Fixed

- bun global package updates now work correctly (workaround for bun update -g bug)

## [1.1.1] - 2025-12-22

### Fixed

- Preserve local changes when syncing directory configs

## [1.1.0] - 2025-12-14

### Added

- uv package manager support for Python tools
- Beta release support with versioned Homebrew formulae

### Fixed

- Homebrew versioned formula conflicts
- Auto-resolve manifest conflicts during rebase
- Retry push on rejection, reset on rebase conflict

## [1.0.4] - 2025-12-08

### Fixed

- Vendor OpenSSL for cross-compilation

## [1.0.3] - 2025-12-07

### Added

- Code signing and notarization for macOS binaries
- Deferred cask installation

### Fixed

- Split pull into fetch+rebase to avoid multi-branch errors
- Install launchd service on init for auto-start on reboot

## [1.0.0] - 2025-12-01

### Added

- Initial release
- Dotfile syncing across machines
- Package manager support: brew, npm, pnpm, bun, gem
- Encrypted secrets with passphrase-based keys
- Background daemon with periodic sync
- Team sync for shared configurations
