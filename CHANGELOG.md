# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Package manifests now record the version each machine has installed: `name@1.2.3` in `npm.txt`, `pnpm.txt` and `bun.txt`, `name==1.2.3` in `uv.txt`, and `name:1.2.3` in `gems.txt`. When machines differ, the manifest keeps the newest version, by each ecosystem's own ordering: semver for npm, pnpm and bun, PEP 440 for uv, so `1.0rc1` is older than `1.0`, and RubyGems rules for gem, so `1.0.pre` is older than `1.0`. A new machine installs that exact version. Lines without a version still work and install the newest release that passes the release-age limit. The Brewfile stays unpinned, because Homebrew installs only the current release. A version must be a concrete release in the ecosystem's format: semver for npm, pnpm and bun, PEP 440 for uv, and a RubyGems version for gem. Tether skips a line or record entry with a dist-tag such as `latest` or a range, and shows a warning
- New approval inbox in `~/.tether/inbox.json`. It stays on this machine and is never synced. Use `tether packages inbox` to list held packages, and `tether packages approve <id>` or `tether packages reject <id>` to decide. In a terminal, `approve` and `reject` show the item and ask. Without a terminal, name the version, Homebrew tap or key fingerprint you reviewed, for example `tether packages approve npm:example 1.0.0`. Tether refuses the decision when the item no longer has it. An approved package installs at once. A rejected version, tap or key is not offered again. A decision covers only that version, for a Homebrew formula or cask only the tap its name resolved to, trusted or not, and for a machine only that key. A different version, tap or key waits for a new decision, and a changed key still shows its warning. Approving a Homebrew package from an untrusted tap does not trust the tap: approve the tap item or add it to `packages.brew.trusted_taps`. Approval applies to the item as Tether showed it. If a sync changed the item since then, for example its key, version or tap, Tether refuses the approval and asks you to review the item again. When a sync changes a held item in such a way, Tether reports it again as a new item
- `tether sync` in a terminal now asks about held packages. The daemon holds them and sends one notification for each new batch
- New setting `packages.auto_install_from_trusted` (default true)
- Each machine now has an SSH signing key in `~/.tether/signing_key`. Tether makes it on `tether init`, or on the first sync after an upgrade. Each sync signs this machine's record `machines/<id>.json` with it, in `machines/<id>.json.sig`. The signature covers the machine id and the exact bytes of the record. Tether also signs the commits it makes in the personal sync repo, and `git log --show-signature` can verify them. Commit signatures are an audit trail only
- New trust store in `~/.tether/trusted_keys`, a TOML file with one entry per machine id: its public key and the key's fingerprint. It stays on this machine and is never synced. It starts with this machine's own key. A key that signs a new machine's record waits in the approval inbox as "trust machine". Approve it with `tether packages approve machine:<id> <fingerprint>` or `tether machines trust <id> <fingerprint>`. The command trusts the key only if its fingerprint matches. Without a fingerprint, it shows the current one and asks, and only in a terminal. `tether machines untrust <id>` removes it
- `tether machines list` now shows each machine's key fingerprint and whether this machine trusts it
- Tether now checks synced npm, pnpm, bun, uv and gem packages against OSV before it installs them. A `MAL-` advisory blocks the install, and the package waits in the inbox, where it cannot be approved. Other advisories show a warning and are stored with the inbox item. A network failure does not block installs. Tether uses `curl` with a 10-second limit. Without a pinned version, only `MAL-` advisories count
- The dashboard has a new Security tab for the approval inbox. It shows each held package with its reasons, source machine, commit and OSV advisories, and each machine key that waits for trust with its fingerprint. A changed key shows a loud warning. Press `a` to approve and install or to trust a key, `x` to reject, or `A` to approve all packages that are not malicious. The tab also lists the trusted machines. The header shows the number of held items. Approve all approves only the packages its question listed. `a` and `x` act on the item as the screen last showed it. If a refresh changed it since then, for example its version or key, Tether refuses and shows the new item first. Ctrl+K lists held items too, but it only opens them on the Security tab, so you see the fingerprint and warnings before you decide

### Changed

- A package from the manifests no longer installs on its own unless a trusted machine record lists that exact package and version. A record is trusted when its signature verifies against the key this machine trusts for that machine id. The manifests and commit signatures do not count. Other packages wait in the approval inbox. Tether checks this again on every sync. It checks every held package again too, whatever held it. A package installs, and its inbox item goes, once no check holds it: for example when a record that lists it becomes trusted, npm can enforce the release-age limit, or its tap becomes trusted. When the reasons change, Tether reports the item again. Set `packages.auto_install_from_trusted = false` to hold packages from trusted records too. The manifest only names the package: a sync installs the newest version that a trusted record lists, whatever version the manifest pins, so nobody can pick an older version that a trusted record once listed. `tether rollback` installs the newest trusted version too, unless you confirm the snapshot's version in a terminal
- Synced packages that the installed manager cannot hold to `packages.min_release_age_days` now wait in the approval inbox. Before, they installed after a warning. This covers gem and old npm, pnpm and bun
- Homebrew taps that are not trusted, and formulae and casks from them, now wait in the approval inbox. Before, Tether skipped them with a warning. A sync also taps a new tap only when a trusted machine record lists it, as for packages. Other taps wait in the inbox, even trusted ones

### Security

- Dashboard installs now check OSV first. A `MAL-` advisory blocks them, and so does a package the inbox holds as malicious. When OSV cannot be reached, the dashboard asks before it installs, and only `y` installs. Approvals on the Security tab, one or all, ask the same question before they approve, and the question names each package OSV could not check. `tether packages approve` checks OSV before it approves too. When OSV cannot be reached, it asks in a terminal, and without a terminal it fails and approves nothing. Rollbacks get the checks of `tether rollback`
- When a trusted machine signs its record with a different key, Tether shows a warning and asks through the inbox. Until you approve the new key, Tether does not trust that record
- Trust never moves to another machine id on its own. A renamed machine waits in the inbox as a new machine, even with a key that this machine trusts under the old name. `tether machines rename` now renames only the machine it runs on. It builds the record from `~/.tether/machine.json` and signs it again under the new id, under the sync lock. It refuses to rename another machine, because only that machine can sign its record
- A machine record in `machines/` must have a valid machine id (letters, digits, `.`, `_` and `-`) as its file name, and the same id inside it. Tether ignores other records and shows a warning. Tether counts its own record only when this machine's key signed it. A signed record that has entries Tether drops, such as a version range, does not count as signed
- This machine now builds its record from its own copy in `~/.tether/machine.json`, not from the copy in the sync repo. Anyone who can push could add removals or ignored files to the repo copy, and this machine would then keep and sign them. On the first sync after an upgrade, Tether reads the repo copy once, if this machine signed it or an earlier build wrote it without a signature. After this machine saves its first signed record, Tether never reads an unsigned repo copy as its own. If `~/.tether/machine.json` is missing, Tether reads the repo copy only when this machine's key signed it and its generation is not older than the last one this machine saved. The dashboard shows this machine's live package list but no longer writes it to the record. Only a sync writes the package names, and it reads them together with their versions
- Each machine record now has a `generation` that grows with every save, and the signature covers it. Tether keeps the newest generation it has accepted from each machine key in `~/.tether/record_generations.json`. An older signed record from the same key, such as one restored from git history, no longer counts and shows a warning. A different record with the generation already accepted does not count either. Every save of this machine's record, from a sync, `tether ignore` or a dashboard install, holds the sync lock and counts on from the newest saved generation, so two saves never share a generation. Approving a machine's new key still replaces its old key
- Tether now checks every package name from a manifest before it runs a package manager. It skips names that look like flags, URLs, paths, tarballs or `git+`/`github:`/`file:`/`link:` specs, and shows a warning
- New setting `packages.min_release_age_days` (default 7, 0 turns it off). npm, pnpm, bun and uv installs and upgrades skip releases newer than this. The daemon does not auto-upgrade a manager that is too old to enforce it (npm before 11.10, pnpm before 10.16, bun before 1.3, and gem), and it logs a warning once
- npm, pnpm and bun now install and upgrade with install scripts turned off. List packages that need their scripts in `packages.allow_scripts`
- Homebrew taps outside `homebrew/*` must be listed in `packages.brew.trusted_taps`. Tether skips other taps, and formulae and casks from them, and shows a warning
- Tether now rejects package names and versions that a package manager reads as a local file. The check ignores case. It covers `.tgz`, `.tar` and `.tar.gz` for npm, pnpm and bun, `.gem` for gem, `.rb`, `.json` and bottle tarballs for Homebrew, and `.tar.gz`, `.whl` and `.zip` for uv
- Tether now runs every package manager in the empty directory `~/.tether/run`. Before, a package manager ran in your current directory and could install a local file or read project config from it. `gem install` and `gem update` also use `--remote`, so gem never installs a `*.gem` file
- pnpm 11 and later now get `--config.minimum-release-age-strict=true` with the release-age cutoff. Without it, pnpm 11.0 to 12.2 could install a too-new version and add it to `minimumReleaseAgeExclude`
- Tether skips `pnpm update` for packages with scripts off on pnpm 12.0.0 to 12.3.1, because those versions reject `update --ignore-scripts`. It shows a warning once
- `packages.allow_scripts` now runs scripts only for the listed package. pnpm 10.4 and later install it with `--allow-build=<name>`, and npm 12 and later with `--allow-scripts=<name>`. On older npm or pnpm the listed package also installs with scripts off, because those versions would run the scripts of all its dependencies too. Tether shows a warning once
- uv now gets the release-age limit as a duration (`--exclude-newer "7 days"`) on uv 0.9.17 and later. uv saves the limit in each tool receipt. A saved timestamp kept later upgrades at that date, but a saved duration stays relative. With `packages.min_release_age_days = 0`, uv 0.11.24 and later get `--exclude-newer false`, which clears a saved limit. On older uv, run `uv tool install --force <name>` to clear it
- Tether now looks up the tap of a short Homebrew name, such as `bun`, before it installs it. brew can resolve a short name to any tapped repository, so a name from an untrusted tap is skipped like a qualified one. The lookup asks brew only about the core tap and reads other taps' file names, because `brew info` runs a formula's Ruby code
- `tether upgrade` now asks before it upgrades a manager that cannot enforce `packages.min_release_age_days`, such as gem or an old npm. Without a terminal, it skips that manager
- Homebrew upgrades now upgrade only outdated formulae and casks from trusted taps. Before, Tether ran a plain `brew upgrade`, which also upgraded packages from untrusted taps. Tether reads the tap of each installed package from its install receipt and names only trusted packages to brew, because brew runs the Ruby code of each package it loads. Tether no longer runs `brew update`, because it loads every installed package after it fetches the taps. Tether updates trusted taps with `git pull --ff-only`, and brew refreshes its own data for the core taps. Run `brew update` yourself to update Homebrew
- npm, pnpm, bun, uv and gem upgrades, from the daemon or `tether upgrade`, now check each target version against OSV first. A package whose target has a `MAL-` advisory keeps its installed version and waits in the inbox. Tether reads the targets from `npm outdated -g --json`, `pnpm outdated -g --format json`, the `bun outdated -g` table, `uv tool list --outdated` and `gem outdated`. Homebrew has no OSV data, so brew upgrades are not checked
- `tether rollback` now lists each package and version it would install and asks before it changes anything. Before, it ran a full sync first, so a declined rollback still left that sync's installs and dotfile changes. It no longer syncs before it asks. It records each package it removes in this machine's record, so the next sync does not install it again. Without a terminal it needs `--yes`. A snapshot version other than the newest trusted one installs only when you confirm that version in a terminal. `--yes` does not confirm it. It then checks the packages it would install like a sync does, and a version you confirmed counts as approved. Packages that fail a check wait in the inbox, OSV included. It holds the sync lock for the whole rollback, so the daemon cannot sync between its steps. The dashboard relies on these checks: it runs `tether rollback --yes` after its own question and no longer checks OSV itself
- `tether packages approve` now takes the sync lock while it installs, so the daemon cannot install the same package at the same time. `tether upgrade` takes it too, because it queues malicious upgrade targets in the inbox

### Fixed

- A uv tool that Tether installed at a pinned version now upgrades again. uv saves `name==version` in the tool receipt, so Tether installs the bare name a second time to drop that pin and keep the installed version
- A pnpm package that Tether installed at a pinned version now upgrades again. pnpm saves the exact version as the range, so upgrades now run `pnpm update -g --latest`, which ignores the saved range and still applies the release-age limit. pnpm upgrades can now cross major versions, like npm upgrades of global packages

### Changed

- The dashboard has a new look. It uses Catppuccin Mocha or Latte colors when the terminal supports true color, and it picks one from the terminal background. Set `dashboard.theme` to `mocha`, `latte`, `ansi` or `auto` to choose. Terminals without true color keep the 16-color theme
- The dashboard header shows the machine, the daemon state and a spinner while a sync runs. The Overview tab shows a chart of sync commits per day
- The Machines tab shows a card for each machine with its last sync, online/idle/stale state, OS and tether version
- File and manifest diffs in the dashboard show line numbers and colored added and removed lines
- Dashboard messages appear as notices in the top-right corner and disappear after a few seconds. Package warnings from background work, such as an incomplete OSV check, appear there too and go to the log, instead of printing over the screen
- The dashboard supports the mouse: click a tab, a row or a key hint, and scroll lists with the wheel
- Press Ctrl+K in the dashboard to search actions, tabs, files and packages
- The dashboard redraws only when something changes, so it uses almost no CPU when idle
- In the package import list, the "Install?" question now shows above the list. Before, the list hid it

## [1.13.1] - 2026-10-02

### Fixed

- A daemon installed before 1.12.1 used macOS's default `PATH`, so package installs used system tools such as Ruby 2.6 `gem` and failed slowly while holding the sync lock. Running `tether sync` in a terminal now updates the daemon service with your shell's `PATH`. It keeps the installed binary, and it does not start a service that you unloaded
- `tether sync` and `tether resolve` in a terminal now wait for a running sync to finish and show a message. Before, they failed after 2 seconds

## [1.13.0] - 2026-10-01

### Added

- New per-dotfile option `on_conflict = "prompt" | "local" | "remote"` (default `prompt`). Use `local` or `remote` for files that an app rewrites on every machine, such as timestamps or caches. Tether then settles their conflicts without a prompt or a notification

### Fixed

- The daemon no longer sends the same conflict notification every 5 minutes. It notifies once for each newly conflicted file, and the notification names the file
- `tether resolve` choices now stick. Before, "Keep local" and "Merge" were detected as the same conflict again on the next sync
- `tether status` shows when a conflict was first detected, not the time of the last sync

## [1.12.4] - 2026-09-28

### Fixed

- Team and collab commits are no longer lost when they conflict with remote changes. Tether reset these repos to the remote and discarded the local commits. It now keeps them on a `tether-discarded-<time>` branch, keeps uncommitted changes in `git stash`, and shows a warning and a macOS notification with the branch name
- A dirty sync repo no longer counts as a conflict when Tether pulls
- A team commit left by a failed push is pushed on the next sync, after the secret scan, even if nothing else changed
- Purging project history with `git filter-branch` also removes the secrets from other local branches

## [1.12.3] - 2026-09-28

### Fixed

- Fixed regressions from 1.12.2 that could overwrite local edits:
  - The daemon could overwrite local edits to a new collab secret with the remote version on every sync
  - Imported collab secrets, project configs and team secrets could roll back to an older hash, so an unchanged file looked edited and was pushed over newer remote content
  - After a push failed from a network error, a file changed back to its earlier content could be overwritten on the next sync
  - A new file that was never pushed could be overwritten by the remote copy without a conflict prompt
- A commit left by a failed push is now pushed on the next sync, even if nothing else changed. Before, that sync skipped the push and marked the files as synced

## [1.12.2] - 2026-09-28

### Fixed

- A push rejected with `cannot lock ref` is now retried. GitHub reports a push race between machines this way, so one machine could fail every sync while others kept pushing. Retries wait a random delay, then pull, so the later machine builds on the earlier push
- Local edits are no longer lost when a push races with another machine that changed the same file. Tether reset the sync repo to the remote and then treated the edits as synced, so the next sync overwrote them with the remote version and showed no conflict. The next sync now reports a conflict, and a file that only this machine changed is exported again

## [1.12.1] - 2026-09-26

### Fixed

- The launchd daemon now runs with the `PATH`, `GEM_HOME` and `GEM_PATH` of the shell that installed it. launchd's default `PATH` hid Homebrew, so the daemon skipped brew, npm, bun, pnpm and uv, and ran `gem` with the macOS system Ruby. Run `tether daemon install` again to apply
- `gem` installs and updates respect `GEM_HOME`. `--user-install` is only used when `GEM_HOME` is not set, because it overrides `GEM_HOME` and puts executables outside `$GEM_HOME/bin`

### Changed

- The gem manifest records only top-level gems. Default gems that ship with Ruby and gems that are only dependencies are no longer listed, so a sync no longer installs every dependency one by one

## [1.12.0] - 2026-09-01

### Added

- Packages tab manifest history: `h` on a manager lists its manifest commits and `Enter` shows the diff for an entry
- `tether rollback packages <manager> <commit>` rolls a package manager's installed set back to an earlier manifest snapshot, then re-syncs so the removals are recorded
- Dashboard: `R` on a history entry confirms the install/uninstall counts and runs the rollback. Homebrew is not supported yet; its history is still viewable

### Fixed

- Onboarding a machine that shares a hostname with an existing machine no longer overwrites that machine's sync state — machine identity is now a random id rather than the hostname
- `pnpm` failures now surface the real error text (pnpm writes errors to stdout, not stderr, so failures previously showed a blank reason). Both streams are reported, so a Node warning on stderr cannot hide the error
- The dashboard reports a failed background `sync` or `rollback` instead of returning to the normal view as though it had worked

### Changed

- New machines get a random machine id; existing machines keep their hostname-based id, so no migration runs on upgrade. A fleet that already contains two machines sharing a hostname is not auto-repaired — on one of them run `tether machines rename <hostname> <new-name>`; the other machine recreates its own record on its next sync, with empty removed-package and ignore lists
- Sync commits are authored with the machine hostname, so `tether history` stays readable for machines with random ids

## [1.11.10] - 2026-04-08

### Fixed

- Dotfiles with no sync history no longer treated as conflicts (prevented daemon from syncing new files)
- `create_if_missing` dotfiles now receive remote content on first sync even when an app created a default locally
- `effective_dotfiles()` merges profile + global dotfiles instead of replacing (global entries no longer silently dropped)
- `effective_dirs()` now merges profile + global dirs to match `effective_dotfiles` behavior
- `KeepLocal` conflict resolution now clears conflict state (no longer re-prompts on every sync)

### Changed

- `detect_conflict` takes pre-computed hashes so the local file is read once per sync instead of twice
- Extracted `backup_and_write_dotfile` helper to dedupe backup+write logic in `decrypt_from_repo`

## [1.11.9] - 2026-04-07

### Fixed

- Collab join now fails closed when GitHub API collaborator check fails
- Recipient filtering enforced against authorized list during `collab add` and `collab refresh`
- Secret scan during `team add` is now recursive (catches secrets in subdirectories)
- Secure file permissions on Windows for key cache, identity cache, and decrypted secrets via `icacls`
- Centralized `write_owner_only` helper fixes pre-existing file permissions on Unix

### Changed

- Dashboard TUI palette brightened for better readability on dark and light terminals

## [1.11.8] - 2026-03-09

### Fixed

- Directory-based dotfiles (`dotfiles.dirs`) now appear in dashboard Files tab
- Deleted file detection includes `configs/` tracked files

## [1.11.7] - 2026-02-24

### Fixed

- Cross-profile import prompt no longer re-asks for dismissed files on every sync
- `tether sync --rediscover` flag to reset dismissed imports and re-prompt

## [1.11.6] - 2026-02-23

### Added

- Graceful shared↔profile dotfile migration during sync (git mv preserves history)
- Dashboard `t` key to toggle shared flag on dotfiles
- Profile edit prompts for shared flag per dotfile
- Profile name shown in `tether status`

### Fixed

- New profiles start empty instead of cloning dev's dotfile list

## [1.11.5] - 2026-02-23

### Fixed

- New profiles detect locally installed package managers instead of cloning dev's list

## [1.11.4] - 2026-02-23

### Fixed

- New machines joining a flat-only repo now correctly migrate dotfiles to profiled layout

## [1.11.3] - 2026-02-23

### Fixed

- Profile-specific dotfiles no longer bleed across profiles via legacy flat layout fallback

## [1.11.2] - 2026-02-23

### Fixed

- `tether init` on new machine now prompts for profile selection when synced profiles exist

## [1.11.1] - 2026-02-22

### Fixed

- Legacy `dotfiles/` cleanup no longer blocked by sync recreating the directory
- Dashboard profile picker no longer shows "(none)" option

## [1.11.0] - 2026-02-22

### Added

- Machine profiles: assign named profiles to machines for per-machine dotfile/package control
- Profile-aware sync: dotfiles stored under `profiles/<profile>/` with automatic flat-layout migration
- `tether machines profile` subcommands: set, unset, create, edit, list
- `tether history` command: show file change history from sync repo
- `tether restore git` command: restore dotfiles from git history
- Cross-profile discovery: interactive prompt to adopt dotfiles from other profiles
- Dashboard: file history viewer with inline diffs and restore support
- Dashboard: deleted file detection across profile and flat layouts
- Dashboard: profile picker for machine assignment
- Auto-cleanup of legacy `dotfiles/` tree once all machines are upgraded
- Tether config moved from `dotfiles/tether/` to `configs/tether/` to avoid dotfile path collisions

### Fixed

- `tether diff` and `tether resolve` now use profile-aware repo paths
- Glob patterns in dotfile config (e.g., `.claude/commands/*.md`) correctly migrate from flat to profiled layout

## [1.10.0] - 2026-02-21

### Added

- Preserve executable bit (+x) during dotfile sync using git's native mode tracking

## [1.9.9] - 2026-02-20

### Fixed

- Create parent directories when decrypting dotfiles from sync repo

## [1.9.8] - 2026-02-20

### Fixed

- Daemon sync now uses same code paths as manual sync (was missing config import, directory sync, project configs, collab secrets, team repo push, auto-discover, and backup pruning)
- Daemon package export now uses union-of-all-machines logic instead of local-only
- Decrypted dotfiles written with secure permissions (0o600) in both daemon and manual sync
- Manual sync now persists collab secret state (was lost due to early `state.save()`)
- `mark_synced()` now always runs in daemon, even when only inbound changes occur

## [1.9.7] - 2026-02-15

### Fixed

- Project config sync no longer drops local changes

## [1.9.6] - 2026-02-15

### Fixed

- Project config export now follows symlinks to canonical files
- "Local changes will be pushed" no longer repeats every sync

## [1.9.5] - 2026-02-15

### Fixed

- Track synced state for project configs on import

## [1.9.4] - 2026-02-15

### Fixed

- Track synced state for project configs on import (partial fix)

## [1.9.3] - 2026-02-14

### Changed

- Update all dependencies to latest (age 0.11, git2 0.20, ratatui 0.30, notify 8.2, and others)
- Remove unused `rand` direct dependency

## [1.9.2] - 2026-02-14

### Fixed

- Daemon sync no longer overwrites local changes to team/collab secrets (e.g. `.env.local`)
- Collab secrets now backed up before overwriting, matching team secrets behavior

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
