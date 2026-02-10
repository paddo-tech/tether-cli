# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [1.9.1] - 2026-02-10

### Fixed

- Team project configs no longer repeat "local changes will be pushed" every sync

## [1.9.0] - 2026-02-09

### Added

- Sync repo format versioning with forward-compatibility check
- CLI version tracking across machines (`tether machines list`, `tether status`)
- Exclusive file locking to prevent concurrent sync corruption
- Daemon log rotation (5MB cap)
- Team project secret grouping in dashboard Files tab

### Fixed

- Silent error swallowing in daemon sync (now logged)

## [1.8.0] - 2026-02-09

### Added

- Group files by personal/team sections in dashboard Files tab
- Show git repo URLs in file section headers

## [1.7.1] - 2026-02-08

### Added

- Config list editing, package browser, and expandable machines in dashboard

## [1.7.0] - 2026-02-08

### Added

- Interactive TUI dashboard as default command (`tether` or `tether dashboard`)
- Daemon start/stop toggle from dashboard (`d` key)
- Inline config editor with bool toggles, validated text fields, and list editing sub-views (dotfiles, folders, project paths, file patterns)
- Packages tab with collapsible manager sections showing actual package names
- Package uninstall from dashboard with confirmation popup and background execution
- Expandable machines tab showing hostname, OS, dotfiles, per-manager package counts
- Context-sensitive help overlay with all keybindings
- `relative_time` helper for human-friendly timestamps
- `Output::key_value`, `Output::badge`, `Output::divider`, `Output::diff_line` helpers

### Changed

- `tether status` uses compact layout with relative times instead of tables
- `tether config features` uses `Output::key_value_colored` instead of custom `print_feature`
- `tether diff` uses `Output::diff_line` and `HashSet` for O(1) package lookups
- `tether machines` uses `Output::table_full` helper
- `tether upgrade` shows step counter (e.g. "1/3")

## [1.6.3] - 2026-02-08

### Fixed

- Daemon now syncs and updates uv packages (was missing from daemon loop)
- Team project secrets now written to all checkouts, not just the last discovered
- Gem `get_dependents` no longer truncates names at hyphens
- `should_skip_dir` no longer prunes `.vscode`/`.idea` during project config scanning
- `tether diff` now shows bun and gem package differences
- `is_process_running` correctly checks `ESRCH` instead of `ErrorKind::NotFound`
- Project state key parsing correctly extracts full `host/org/repo` identifier
- Secret scanner regex compiled once via `LazyLock` instead of per-call

### Changed

- Package manager trait provides default `export_manifest`/`import_manifest`/`remove_unlisted`
- Daemon `sync_packages` and `run_package_updates` refactored into loops
- `build_machine_state` and `show_package_diff` refactored into loops
- Extracted `run_tick()` to deduplicate unix/non-unix daemon run loop
- Simplified `deserialize_active_teams` serde visitor to `#[serde(untagged)]` enum
- Removed trivial `encrypt_file`/`decrypt_file` wrappers
- Centralized `home_dir()` helper, replacing ~32 inline occurrences

## [1.6.2] - 2026-02-07

### Fixed

- Daemon sync no longer overwrites local dotfile edits — adds `local_unchanged` guard matching manual sync
- Daemon now expands glob patterns (e.g. `.config/fish/*`) in both decrypt and local-to-repo phases
- Daemon respects `ignored_dotfiles` from machine state
- Daemon clears stale conflicts after successful file apply
- Daemon backs up files before overwriting with remote content

## [1.6.1] - 2026-01-30

### Fixed

- `tether packages` now uses two-step selection (managers first, then packages) to reduce scrolling
- `tether packages` respects `-y` flag for skipping confirmation prompts
- Filter invalid bun package entries (bare `@` causing upgrade failures)

## [1.6.0] - 2026-01-29

### Added

- **`tether packages` command**: List installed packages across all managers (brew, npm, pnpm, bun, gem, uv) with interactive multi-select uninstall. Use `--list` for non-interactive output. Shows dependency warnings before uninstalling packages that other packages depend on.

## [1.5.0] - 2026-01-21

### Added

- **Symlink-based multi-checkout sync**: Multiple checkouts of the same repo now share project configs via symlinks to a canonical location (`~/.tether/projects/`). Edit in one checkout, instantly available in all others without syncing.

### Fixed

- Path traversal validation for canonical project paths
- Atomic writes for canonical file updates

## [1.4.1] - 2026-01-19

### Fixed

- Glob patterns now default to `create_if_missing = true` so files from other machines are synced

## [1.4.0] - 2026-01-19

### Added

- **Glob patterns for dotfiles**: Use patterns like `.config/gcloud/*.json` to sync multiple files
- Path safety validation before glob expansion to prevent traversal attacks
- Warning logs when glob patterns match no files

### Fixed

- Daemon stop now force kills after graceful timeout instead of failing

## [1.3.0] - 2026-01-18

### Added

- **Collab secrets**: Share project secrets with GitHub collaborators (`tether collab init/join/add/refresh`)
- **Feature toggles**: Granular control over sync features (`personal_dotfiles`, `personal_packages`, `team_dotfiles`, `collab_secrets`)
- Package timestamps: Track when manifests were modified and packages upgraded (`tether status`)

### Changed

- Reduced config/state file reloading during sync operations

### Security

- Collab name validation to prevent path traversal attacks
- Symlink validation in team repos to stay within repo bounds

## [1.2.0] - 2026-01-13

### Added

- Auto-migrate personal project secrets to team repo when adding org (`tether team orgs add`)
- New `tether team projects migrate` command for manual migration
- Global `--yes` / `-y` flag to skip confirmation prompts (non-interactive mode)
- Config versioning system to prevent older tether from corrupting newer configs

### Fixed

- Show config version error instead of generic "not initialized" message
- Correct error message for identity unlock command

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
