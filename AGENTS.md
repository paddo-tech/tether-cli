# AGENTS.md

> `CLAUDE.md` is symlinked to this file.

## Overview

Rust CLI that syncs dotfiles and global packages across machines via Git. Daemon runs periodic sync every 5 minutes.

## Commands

```bash
cargo build              # Build
cargo run -- <cmd>       # Run in dev
cargo test               # Test (fast suite; the e2e tests skip)
TETHER_E2E=1 cargo test --test e2e  # End-to-end suite in Docker
cargo clippy -- -D warnings  # Lint (must pass before commits)
cargo fmt                # Format
```

## End-to-end tests

Never run Tether or a package manager against your own `~/.tether` or your own machine. A scratch `HOME` is not isolation: npm, uv, brew and gem act on the whole machine. Anything that can run a package manager runs only inside a container. `tests/cli.rs` runs the binary with an empty `PATH`, so no package manager can start.

`tests/e2e/` is a cargo integration test that uses the `testcontainers` crate. Each test starts a Docker network, a git server container and one container per machine. Most machines have logging shims for brew, npm, pnpm, bun, uv, gem, ruby, curl, notify-send and systemctl (`tests/e2e/docker/shims`). So nothing installs, and no package request leaves the container. The upgrade test uses the `tether-e2e-real` image, with real npm, uv and curl. The images set no git identity, as on a new machine.

- Run: `TETHER_E2E=1 cargo test --test e2e`. One test: `TETHER_E2E=1 cargo test --test e2e trust`. Without `TETHER_E2E=1` or without Docker, each test prints why and passes.
- Setup: the first test runs `tests/e2e/images.sh` once. The script builds HEAD and v1.11.10, v1.12.0 and v1.13.1 from `git archive` in a `rust:1-bookworm` container, into `target/e2e/bin/`. HEAD includes uncommitted changes to tracked files. The script also builds the machine images, tagged by a hash of `tests/e2e/docker/`. Each step skips work that is done. The first run takes a few minutes.
- `TETHER_E2E_FLAP_REF=<git ref>` sets the 1.x binary of `config_flap`, for example a 1.x patch commit.
- Logs: `target/e2e/logs/<test>/<machine>.log` has every command, its exit code and its output. The fleet tests also write `events.log` and `summary.txt`.
- CI runs `cargo test --locked --lib --test cli` on macOS and Linux. The e2e HEAD binary is keyed on every top-level entry of the archive except `tests`, `website`, `fastlane`, `.github` and `*.md`.
- Tests: `fleet` (a mixed 1.x and HEAD fleet; checks a to k are listed in `tests/e2e/fleet.rs`), `config_flap`, `config_changes_merge`, `sync_without_a_terminal_skips_conflicts`, `trust_pulls_a_new_record`, `trust`, `inbox`, `rejected_push_without_git_identity` (two HEAD machines without a git identity push at the same time), `upgrade_never_downgrades`, `casks_never_import_on_linux`, `systemd_install_needs_a_user_session`, `notify_send_once_per_inbox_batch` and `cli_contract`.

## CLI Commands

| Command | Description |
|---------|-------------|
| `init` | Initialize Tether on this machine |
| `sync` | Manually trigger a sync (`--dry-run` changes nothing) |
| `status` | Show current sync status, inbox and conflict counts (`--json`) |
| `diff` | Show differences between machines |
| `config` | Manage configuration and feature toggles |
| `daemon` | Control the background daemon: `start`, `stop`, `restart`, `status`, `logs -f -n`, `install`, `uninstall` |
| `machines` | `list` (`--json`), `show`, `rename <NEW>`, `remove`, `trust --fingerprint`, `untrust`, `profile` |
| `packages` | `list` (`--json`, `--other-profiles`, also bare `packages`), `inbox` (`--json`), `approve [--all] [--from] [--expect]`, `reject`, `install`, `share --to`, `unshare --from`, `uninstall` (alias `remove`) |
| `ignore` | `secrets add/list/remove` (secret scanning), `files add/project/list/remove` (files this machine keeps) |
| `team` | Manage team sync (dotfiles, secrets, projects) |
| `resolve` | Resolve file conflicts |
| `unlock` | Unlock encryption key with passphrase |
| `lock` | Clear cached encryption key |
| `upgrade` | Upgrade packages without downgrades; asks, or needs `-y` (`--dry-run` lists) |
| `restore` | Restore files from backup |
| `rollback` | Roll back a manager's packages to a manifest commit (not brew) |
| `history` | Show a dotfile's history in the sync repo |
| `identity` | Manage age identity for team secrets |
| `collab` | Collaborator-based project secret sharing |

CLI rules: every error exits 1 with one `Error:` line on stderr. A clap usage error, such as an unknown flag or a missing argument, exits 2, as is the convention. Every prompt goes through `cli::Prompt`: `-y` confirms and takes defaults, and without a terminal a prompt fails and names `-y`. `-y` never approves an inbox item or trusts a key, with one exception: `packages approve --all -y` approves the listed packages. It still leaves machine keys, malicious packages, packages whose record fails its signature and packages without a version or tap to name. A review that approves or trusts uses `Prompt::review`, which `-y` never answers. CLI text styles through `cli::output::Colorize`, never `owo_colors` directly, so pipes and `NO_COLOR` get plain text. Machine arguments take an id or a hostname of exactly one trusted machine or this machine (`machines::resolve`). `machines trust` also takes the hostname of a new machine whose record a key signs. An unknown name is an error. Package ids are `manager:name`; `brew:` and `cask:` stand for `brew_formulae:` and `brew_casks:`. `--json` is experimental in 2.0.

## Key Dependencies

- **clap** - CLI parsing
- **tokio** - Async runtime
- **git2** - Git operations
- **inquire** - Interactive prompts
- **owo-colors** - Terminal colors
- **aes-gcm** - Encryption
- **age** - Passphrase-based key encryption
- **ssh-key** - Commit signing keys and SSH signatures

## Feature Toggles

Managed via `tether config features`. Available toggles:

| Feature | Default | Description |
|---------|---------|-------------|
| `personal_dotfiles` | true | Sync personal dotfiles |
| `personal_packages` | true | Sync personal package manifests |
| `team_dotfiles` | false | Sync team dotfiles |
| `collab_secrets` | false | Enable collab secret sharing |
| `team_layering` | false | Merge team + personal dotfiles |

## Data Layout

**~/.tether/**

Nothing in it syncs as a dotfile or a dir, in either direction. Paths compare after symlinks and aliases (such as /var for /private/var) resolve, and case-insensitively on macOS (`config::TetherDir`).

- `config.toml` - Main config (versioned, synced encrypted). Every write sets mode 0600 (`atomic_write_private`), also on a file that was 0644. A symlinked config.toml stays a symlink: `atomic_write` writes its target, and refuses a symlink to a missing file. A sync merges the synced copy into it, three-way (`src/sync/config_merge.rs`). The exported copy has `config_writer = 2` and `config_generation`; 1.x drops both when it saves, so a synced config without the marker is a 1.x copy. `Config::save` and the merge edit the file in place, so comments and unknown keys stay. A new table under an inline table or dotted keys goes in as an inline value, and an empty new table keeps its header
- `config.base.toml` - The base of the next merge: the synced config.toml of the last merge, or of the last export once its push succeeded (never synced, mode 0600). `config.base.pending.toml` holds an export until the push; the next sync deletes it. Without a base, or when it does not read: the local file, when its hash is the one `state.json` `files` recorded at the export; else the copy in the sync repo's history whose hash is `config_export_hash` (or the recorded hash, as 1.x and earlier betas wrote it); else no base. A machine with no recorded hash takes its local file as base, so it joins the fleet's config
- `local.toml` - Settings for this machine only (never synced). `[packages] min_release_age_days` and `[merge] command`/`args` override config.toml on this machine
- `state.json` - Sync state, including `install_failures` (synced packages that failed to install here; retried after 24h or on a new version), `profile_notice_shown` (the one-time notice of packages other profiles have), `config_generation` (the newest config generation merged or exported here), `config_export_hash` (SHA-256 of the last exported config copy) and `config_error` (the synced config copy that did not read, once warned)
- `sync/` - Personal sync repo
- `teams/<name>/` - Team sync repos
- `collabs/` - Collab project configs
- `identity.pub` - Age public key
- `daemon.pid` - Daemon process ID
- `daemon.log` - Daemon logs
- `backups/` - File backups
- `conflicts.json` - Conflict state
- `inbox.json` - Approval inbox (never synced)
- `inbox.lock` - Lock for inbox and trust store changes
- `machine.json` - This machine's last record (never synced). Its removals and ignores carry into the next record; the repo copy is never read for them
- `record_generations.json` - Newest record `generation` accepted per machine key fingerprint, with the SHA-256 of that record (never synced). A signed record from the same key with a lower generation, or a different record with the same generation, is a replay and grants no trust
- `run/` - Empty working directory for package managers
- `signing_key` - This machine's ed25519 SSH key for record and commit signatures (0600)
- `trusted_keys` - Trusted machine keys (never synced). TOML: `version = 1`, then `[machines."<id>"]` with `public_key` (OpenSSH line) and `fingerprint` (`SHA256:...`). Earlier builds wrote git allowed signers lines; Tether reads them and rewrites the file as TOML on the next trust change

**Sync repo structure:**
- `dotfiles/` - Dotfiles
- `configs/` - App configs
- `manifests/` - Package manifests: one package name per line, never a version, as 1.x reads them. Versions come only from signed machine records (`package_versions`). Pinned lines from pre-release 2.0 builds still parse, and the next write drops the version
- `machines/` - Machine-specific state (`<id>.json`) and its signature (`<id>.json.sig`, sshsig, namespace `tether-machine`, over `tether-machine-v1\n<id>\n<sha256 of the json bytes>\n`). Only a record whose signature verifies against the trusted key for its id lets packages auto-install. Trust is transitive: a signed record lists the packages its machine installed, so trusting a machine trusts what it installed. A local rejection still blocks a package
- `packages/profiles.toml` - Package membership: `[profiles]` maps `"manager:name"` to the profile names the package belongs to. Plain TOML, sorted, names only. 1.x never reads or writes it
- `projects/` - Project secrets

## Package profiles

- Each package belongs to one or more profiles. A 2.0 machine installs a synced package only when its profile is a member, and a trusted record lists the package. The version still comes from trusted records of any profile.
- Without an entry in `packages/profiles.toml`, the members are the profiles of this machine and of the trusted machines whose signed records list the package. The profile comes from the signed `profile` field of `machines/<id>.json`. A record without one counts as the default profile `dev` (`DEFAULT_PROFILE`), which is also the profile of a machine with no assignment.
- An entry is authoritative: `"npm:typescript" = ["dev", "server"]`. Ids are canonical (`membership::canonical_id`): brew formulae and casks without their tap, uv names PEP 503 normalized, npm, pnpm, bun and gem names as written. Entries that name one package two ways merge on read, and the next write keeps one.
- A file that exists but does not parse is an error, never an empty table. A sync then installs no synced packages (warning, one daemon notification per error in `state.json` `membership_error`), and `share`/`remove` refuse.
- An included formula or cask named `owner/repo/name` brings tap `owner/repo` into scope, for the import and the inbox.
- Records that are not trusted count only for a package no trusted record lists. Such a package can only wait in the inbox, so a forged profile cannot widen a trusted package.
- This machine's own record always counts, so a machine never loses what it installed.
- `tether packages share <manager:name> --to <a,b>` adds profiles to the current members. `tether packages remove <manager:name>` uninstalls the package here, then takes this machine's profile out of the entry. A failed uninstall changes no membership. When this profile is the only member, it is a plain uninstall. `remove` refuses `brew_taps`. `share --to` and the checklist accept the default profile and current members even when config.profiles does not define them. The dashboard Packages tab does the same with `t` (profile checklist) and Enter (uninstall).
- A sync drops inbox items for packages that leave this machine's profile.
- `membership::save_edit` applies an `Edit` (profiles to add and remove) to one entry. The caller holds the sync lock: the CLI waits for it, the dashboard does not. It refuses a dirty sync repo or unpushed commits. Each of up to three tries fetches, hard-resets to origin/main, reads the table again, applies the delta to the fresh members, commits and pushes once. Edits from other machines survive, also to the same package. An edit that changes nothing writes nothing. A failed commit or push resets to origin/main and removes a file the remote lacks. With `seen` (the dashboard checklist), it refuses when the fresh members differ from those the user saw.
- Integrity limit: anyone who can push can edit the file and widen membership. A package still installs only when a trusted signed record lists it, so the edit only moves already-trusted packages between profiles.
- Removals stay per machine (`removed_packages`). `packages.remove_unlisted` is not wired to any code path, so no machine uninstalls packages that other profiles list.
- Code: `src/sync/membership.rs`; `import_packages` filters manifest names through it.

## Compatibility with 1.x

- Rolling upgrades work one machine at a time. A sync repo can hold 1.x and 2.0 machines together.
- The 2.0 protections (trust, release age, OSV, signatures) apply only on upgraded machines. A 1.x machine installs without them. Its unsigned record grants no trust, so its packages wait in the inbox of 2.0 machines.
- Package profiles apply only on 2.0 machines. Manifests stay the union of all records, so a 1.x machine still installs the packages of every profile. Profile definitions and `machine_profiles` stay in config.toml. A 1.x machine exports its stale copy of config.toml after it applies a remote one, and 1.x writes its maps in random order. So on 1.x machines a config.toml change can revert (including `machines profile set`), and with several 1.x machines config.toml can flap for some rounds. 2.0 merges config.toml instead, on raw TOML values (`src/sync/config_merge.rs`). Merge rules:
  - A missing key holds its default before values compare, so a format-only rewrite or a written-out default is no change.
  - Each setting merges against the base, the synced copy of the last merge or export. A setting one side changed takes that side. When both sides changed it, the local value stays with a warning. The export makes the base the exported copy, so the fleet settles on the last exporter within two syncs.
  - Maps merge per key. `machine_profiles` and `profiles` always merge per entry. A machine's own `machine_profiles` entry keeps its local value. A profile that a surviving assignment names is never deleted (the local or base definition stays, with a warning).
  - Only the lists in `SET_LISTS` merge as sets, item by item (dotfile entries keyed by path), and are written sorted: `dotfiles.files`, `dotfiles.dirs`, `packages.allow_scripts`, `packages.brew.trusted_taps`, `profiles.*.dotfiles/dirs/packages`, `team.orgs`, `teams.allowed_orgs`, `teams.teams.*.orgs`, `teams.collabs.*.projects`. Every other list (`merge.args`, `teams.active`, `project_configs.search_paths/patterns`, `members_cache`, unknown lists) merges as one value. Of two set-list items with one key, the richer stays, whatever their order: a table over a bare string, more fields over fewer.
  - A synced copy without `config_writer` came from 1.x: a key it lacks keeps the base value, so a 1.x save never deletes 2.0 settings. The same holds in set-list items: an item that 1.x wrote without a field keeps the field of the base item. With the marker, a missing key that `Config` skips when it holds its default (`SKIPPED` in `config_merge.rs`, such as `allow_scripts`) holds that default, so a cleared list stays cleared. A test ties `SKIPPED` to every list `Config` skips.
  - Every export goes through `config_merge::export_text`. It adds `config_writer = 2`, `config_generation` and every key that `Config` writes and the local file leaves out, so the copy has every field 1.x requires, such as `packages.*.sync_versions`. The generation is one more than the newest this machine knows: of the base, the local file, the repo copy and `state.json` `config_generation`. The export also replaces a repo copy without the marker, so a 1.x save is marked again after one export.
  - 1.x pushes its copy of an earlier 2.0 export again verbatim, marker and generation included. A synced copy with the marker and a generation below the newest one this machine merged or exported is stale: the sync ignores it, and the export restores the current config with a new generation. An equal generation (two machines exported at once) and a higher one (this machine was offline) merge as usual. A copy without the marker always merges, so a 1.x user who sets a value back keeps the change.
  - The export becomes the base only after its push. A pull that discards the export commit merges against the base before the export, so local edits stay local edits.
  - A synced config with a newer `config_version` leaves the local config alone and the export does not overwrite it. A synced config that does not decrypt or read skips the merge and the export of that sync, with one warning per copy; the rest of the sync runs.
  A machine that never synced its config takes the remote one, except its own assignment. 2.0 writes maps sorted, so 2.0 machines settle. Change config.toml before 1.x machines join or after they upgrade. A machine sets only its own profile: the merge keeps this machine's `machine_profiles` entry, whatever another machine or a 1.x copy holds. The warning that the synced config changed this machine's profile can fire only when the sync copies the synced config over a missing config.toml, or when the local file changed during the sync. The e2e test `config_flap` shows the flap on 1.x machines. Set `TETHER_E2E_FLAP_REF` to run any commit as its 1.x machine, such as a 1.x patch.
- Manifests stay names-only. Never write `name@ver`, `name==ver` or `name:ver` to `manifests/*`.
- Synced formats change only by additions until 3.0. Do not add `deny_unknown_fields` to a synced struct. Do not remove, rename or retype a field that 1.11.10, 1.12.0 or 1.13.1 requires. New fields take `#[serde(default)]`. Tests in `config.rs` and `sync/state.rs` pin the 1.x shapes.

## Code Quality

Before completing work:
1. `cargo clippy --all-targets -- -D warnings` (zero warnings)
2. `cargo fmt --all`
3. `cargo build --release`

## Releasing

Homebrew tap: `paddo-tech/homebrew-tap`

### Version Bump Checklist

1. Update version in `Cargo.toml`
2. Add entry to `CHANGELOG.md` with date and changes
3. Commit: `git commit -am "chore: release vX.Y.Z"`
4. Push to main - CI creates tag, builds, signs, notarizes, and updates Homebrew formula

The `build` job (self-hosted macOS) signs and notarizes the macOS binaries, creates the tag and the release. The `linux` job then builds static musl binaries for x86_64 and aarch64 in a `rust:1` container and uploads them to the same release. The `homebrew` job waits for both and writes the formula from the release checksums. Each target ships as `tether-<target>.tar.gz` with a `.sha256` file. To move the Linux builds to a self-hosted runner, change the `runner` values in the `linux` matrix. The runner needs Docker.

**Do NOT create tags manually** - the release workflow handles tagging.

### Versioning

- **Patch** (1.0.x): Bug fixes
- **Minor** (1.x.0): New features, backward compatible
- **Major** (x.0.0): Breaking changes
- **Prerelease**: Use `-beta.N` suffix (creates versioned formula)

Users install via:
```bash
brew tap paddo-tech/tap
brew install tether-cli
```

Homebrew on Linux uses the same commands. Without Homebrew, Linux users download `tether-x86_64-unknown-linux-musl.tar.gz` or `tether-aarch64-unknown-linux-musl.tar.gz` from the GitHub release and put `tether` on `PATH`. The binaries are static and need no particular glibc.
