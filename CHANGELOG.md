# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [2.0.0-beta.3] - 2026-10-08

### Fixed

- A sync no longer exports a machine's config.toml over changes that another machine pushed. Before, a machine that only rewrote its config in another format, as the first sync after an upgrade does, exported its whole copy. Another machine then lost its new profile and its profile assignment, and installed the packages of profile `dev`. Tether now compares settings, not bytes, and merges config.toml against the copy of the last sync, which it keeps in `~/.tether/config.base.toml`. Edits to different settings on two machines both stay. Lists, such as dotfiles, keep the additions of both machines. When both machines changed one setting, this machine's value stays, and the sync warns. A machine always keeps its own profile assignment. A new machine takes the synced config, so its defaults no longer replace the settings of the other machines. A change on a 2.0 machine now stays when a 1.x machine exports its older copy of config.toml
- The warning that the synced config changed this machine's profile no longer says "from dev to dev". A machine without an assignment is in profile `dev`, so the sync now warns only when the profile differs, and names both profiles
- `tether sync` without a terminal, for example from cron or ssh, now runs as the daemon does. Before, a dotfile conflict failed the whole sync with "needs a terminal", and casks that need a password failed and counted as install failures. Now the sync skips the conflict, records it for `tether resolve` and notifies. It defers casks that need a password to the list that the next `tether sync` in a terminal installs. `-y` still answers prompts as before
- The one-time notice about packages of other profiles is now short. Before, it named every package on one line, often hundreds. It now counts them per manager and per profile, and names the commands that list and share them. The new `tether packages list --other-profiles` lists these packages with their profiles, also with `--json`
- `tether machines trust <id> --fingerprint <fp>` now pulls the sync repo when it does not have the machine's signed record yet. Before, it failed with "has no signed machine record" until the next sync. When the record is still missing after the pull, the error says to run `tether sync` on that machine first. The command now takes the sync lock

## [2.0.0-beta.2] - 2026-10-08

### Fixed

- Sync no longer needs a git identity. On a machine without `user.name` and `user.email`, a rejected push failed its rebase. Tether then reset the sync repo and moved the local commits to a `tether-discarded-*` branch, and `tether init` failed. Each git command that Tether runs now gives the identity that Tether commits use, and turns off commit signing for rebased commits. Only a real content conflict now resets the repo. Any other rebase failure keeps the local commits and shows the git error

## [2.0.0-beta.1] - 2026-10-07

### Added

- New commands: `tether packages list` (bare `tether packages` lists too), `tether packages install <manager:name>` to install a package another machine lists, as the dashboard's Import does, `tether packages unshare <manager:name> --from <profile>` to take a profile off a package without uninstalling anything, and `tether packages approve --all [--from <machine>]`. `approve --all` lists the packages it covers and leaves machine keys, malicious packages, packages whose record fails its signature and packages without a version or tap to name in the inbox. It asks first, and needs `-y` without a terminal. It is the only command where `-y` approves inbox items. It approves exactly the listed items, so a package that a sync queues meanwhile waits for its own decision. `packages install` takes the sync lock first, then applies the checks of a sync, except the trusted records: a package that waits in the inbox for any reason needs `approve`, a rejected release or tap stays rejected, a tap or a formula or cask from a tap that is not trusted goes to the inbox, and the release is the newest one older than `packages.min_release_age_days`. A cask that needs a password fails without a terminal. The dashboard's Import runs the same checks and does not list packages that wait in the inbox
- New commands: `tether machines show <machine>` prints a machine's profile, versions, record status, full key fingerprint and trust. `tether daemon status` shows whether the daemon and its login service run, and the last sync and upgrade. `tether daemon logs -f -n <lines>` follows the log. It follows the log across rotation, also when the daemon truncates the log or another file replaces it. `tether status` shows how many items wait in the inbox and how many files have conflicts
- `--json` on `tether status`, `tether machines list`, `tether packages list` and `tether packages inbox` prints one JSON value on stdout, and messages go to stderr. The flag is experimental in 2.0: field names can still change
- `tether upgrade --dry-run` lists what an upgrade would change, and which packages stay because the release-age limit allows only an older version
- `tether machines profile create` takes `--from <profile>` to copy a profile and `--managers <list>` to set its package managers, so a script can create a profile. With `-y`, the wizard takes every default answer
- Packages now belong to profiles. A machine installs only the packages of its own profile, so a server no longer gets the desktop apps of your laptops. A package belongs to the profiles of the trusted machines whose signed records list it. To send a package to another profile, run `tether packages share npm:typescript --to server`. On a server, `tether packages uninstall npm:typescript` uninstalls it, then takes the server profile out of it. The other profiles keep it. Other server machines stop new installs of it, but keep any copy they have. If the uninstall fails, the profiles do not change. `uninstall` does not remove Homebrew taps. A formula or cask from a tap, such as `vendor/tools/thing`, brings its tap to the profiles that have it. Tether keeps these choices in `packages/profiles.toml` in the sync repo, for example `"npm:typescript" = ["dev", "server"]`. Each change starts from the remote branch, adds or removes only the profiles you named, and pushes under the sync lock. So changes from two machines both stay, also for the same package. A change that changes nothing writes nothing. A change does not start while the sync repo has changes that are not pushed. When a commit or push fails, the sync repo goes back to the remote state. The dashboard checklist does not save when another machine changed the package after you opened it. Tether writes each package under one id: a Homebrew name without its tap, and a Python name as PEP 503 normalizes it. Two ids for one package count as one entry, and the next change keeps one. If `packages/profiles.toml` exists but does not read, a sync installs no synced packages and warns, and the daemon notifies once. `share`, `unshare` and `uninstall` stop with an error until you fix or delete the file. Anyone who can push can edit this file, but a package still installs only when a trusted signed record lists it. A machine without a profile counts as profile `dev`. `share --to dev` and the checklist accept `dev` also when config.toml does not define it. The dashboard Packages tab shows the profiles of each package, and `t` opens a checklist to change them. Uninstall on that tab also leaves the other profiles enrolled. `tether status`, `tether packages list` and the Packages tab show this machine's profile. A sync drops inbox items for packages that leave this machine's profile. Only machines on 2.0 apply profiles: the manifests stay the union of all machines, so a machine on 1.x still installs the packages of every profile. A machine on 1.x can revert a profile assignment in config.toml. A sync on 2.0 then warns that this machine's profile changed and names the command that restores it. Machines on 1.x can push config.toml back and forth after any config change, so set profiles before machines on 1.x join or after they upgrade
- Linux release binaries for x86_64 and aarch64; Homebrew on Linux supported. Each release has `tether-x86_64-unknown-linux-musl.tar.gz` and `tether-aarch64-unknown-linux-musl.tar.gz`, with SHA-256 checksums. The binaries are static, so they run on any distribution. `brew install tether` now works on Homebrew on Linux
- On Linux, `tether daemon install` now installs a systemd user service in `~/.config/systemd/user/tether.service` and enables it. The service gets your shell's `PATH`, `GEM_HOME` and `GEM_PATH`, as on macOS. `tether daemon uninstall` removes it. Without a systemd user session, Tether asks you to use `tether daemon start`. Paths with spaces, `$` or `%` work. systemd cannot run a program whose path has a quote, a backslash or a control character, and a unit cannot hold a newline in the log path, `PATH`, `GEM_HOME` or `GEM_PATH`. In these cases `tether daemon install` stops with an error and writes no unit
- New file `~/.tether/local.toml` for settings on this machine only. Tether never syncs it. Set `[packages] min_release_age_days` or `[merge] command` and `args` there to override config.toml on this machine. Without `args`, the merge tool gets `{local} {remote} {merged}`

- Tether now marks a machine record that may be an old id of this machine: a hostname id from builds before random ids. This is only a guess, and you confirm it before Tether removes anything. Check that no other machine uses the hostname. Tether marks a record when all of these are true. Its hostname or its id is this machine's hostname. Its id is not a random id. It is older than this machine's record. It has not synced for 7 days. No other machine's key signed its bytes, even when the record has entries Tether drops. The build that wrote the record does not count, because a machine keeps its hostname id after an upgrade. `tether machines list` and `tether status` show the command that removes it. On the dashboard Machines tab, press `D` or use "Remove old record" in Ctrl+K, then `y`, to remove it and commit the removal. Under the sync lock, the dashboard checks the record again. If a sync changed the record since the question opened, or it no longer looks like an old id, the dashboard refuses and asks you to review it again

- Machine records now list the installed version of each npm, pnpm, bun, uv and gem package, in `package_versions`. A new machine installs a package at the newest version that a trusted record lists. When records differ, the newest version wins, by each ecosystem's own ordering: semver for npm, pnpm and bun, PEP 440 for uv, so `1.0rc1` is older than `1.0`, and RubyGems rules for gem, so `1.0.pre` is older than `1.0`. A version must be a concrete release in the ecosystem's format. Tether drops a record entry with a dist-tag such as `latest` or a range. Without a trusted version, a package installs the newest release that passes the release-age limit
- Package manifests stay in the 1.x format: one package name per line, without versions. So machines on 1.x read them as before. Pre-release 2.0 builds wrote pinned lines such as `name@1.2.3`. Tether still reads such a line as its name, and the next write drops the version. The Brewfile is unchanged
- Machines can upgrade to 2.0 one at a time. The new checks protect only the machines that run 2.0. A machine on 1.x still installs synced packages without them, and its record has no signature, so its packages wait in the inbox of 2.0 machines until it upgrades and you trust it. `tether status`, `tether machines list` and the dashboard name each machine on 1.x in one line. Tether reads the build from the version in the record. A record that names 2.x and fails its signature shows "signature failed (ignored)", not "on 1.x". Until 3.0, Tether changes synced formats only by additions: config.toml, machine records, signatures, manifests and the Brewfile stay readable by 1.11.10, 1.12.0 and 1.13.1
- `tether packages inbox`, the inbox review of `tether sync` and the dashboard Security tab group held items by source machine and reason. A new machine that inherits hundreds of packages shows a few groups. The review offers "Install all from <machine>", and on the Security tab `M` approves all from the selected item's machine. As with approve all, malicious packages, packages from a record that fails its signature, and machine keys need their own answer, and the approval covers only the items the question showed
- New approval inbox in `~/.tether/inbox.json`. It stays on this machine and is never synced. Use `tether packages inbox` to list held packages, and `tether packages approve <id>` or `tether packages reject <id>` to decide. In a terminal, `approve` and `reject` show the item and ask, and `-y` does not answer them. Without a terminal, name the version, Homebrew tap or key fingerprint you reviewed, for example `tether packages approve npm:example --expect 1.0.0`. A tap item binds to its own name. A held package without a version binds to the release it would install now: `approve` shows that release and installs it. An item that has nothing to name, for example when the registry cannot be reached, needs a terminal. A package whose record fails its signature needs a second question in a terminal, also with `--expect`. Without a terminal it needs `--expect` and `--allow-signature-failed`. Tether refuses the decision when the item no longer has it. An approved package installs at once. A rejected version, tap or key is not offered again. A decision covers only that version, for a Homebrew formula or cask only the tap its name resolved to, trusted or not, and for a machine only that key. An approved formula or cask installs by its qualified name, such as `homebrew/core/wget`, so brew cannot pick another tap after you reviewed it. A different version, tap or key waits for a new decision, and a changed key still shows its warning. Approving a Homebrew package from an untrusted tap does not trust the tap: approve the tap item or add it to `packages.brew.trusted_taps`. Approval applies to the item as Tether showed it. If a sync changed the item since then, for example its key, version or tap, Tether refuses the approval and asks you to review the item again. When a sync changes a held item in such a way, Tether reports it again as a new item. A sync drops an item for a package that no machine record lists any more
- `tether sync` in a terminal now asks about held packages. The daemon holds them and sends one notification for each new batch
- New setting `packages.auto_install_from_trusted` (default true)
- Each machine now has an SSH signing key in `~/.tether/signing_key`. Tether makes it on `tether init`, or on the first sync after an upgrade. Each sync signs this machine's record `machines/<id>.json` with it, in `machines/<id>.json.sig`. The signature covers the machine id and the exact bytes of the record. Tether also signs the commits it makes in the personal sync repo, and `git log --show-signature` can verify them. Commit signatures are an audit trail only
- New trust store in `~/.tether/trusted_keys`, a TOML file with one entry per machine id: its public key and the key's fingerprint. It stays on this machine and is never synced. It starts with this machine's own key. A key that signs a new machine's record waits in the approval inbox as "trust machine". Approve it with `tether packages approve machine:<id> --expect <fingerprint>` or `tether machines trust <id> --fingerprint <fingerprint>`. The command trusts the key only if its fingerprint matches. Without a fingerprint, it shows the current one and asks, and only in a terminal. `tether machines untrust <id>` removes it. `tether machines remove <id>` and the dashboard removal of an old id remove it too, on this machine only, so a later record under that id waits for approval again. Trust is transitive. A machine's signed record lists every package it installed, so trusting the machine trusts those packages, whoever added them on that machine. A package you rejected on this machine stays blocked
- `tether machines list` now shows each machine's key fingerprint and whether this machine trusts it. It checks each record as a sync does, so a record that a sync ignores shows "replayed (ignored)" or "signature failed (ignored)", not "trusted". The key line of the dashboard Machines cards shows the same
- Tether now checks synced npm, pnpm, bun, uv and gem packages against OSV before it installs them. A `MAL-` advisory blocks the install, and the package waits in the inbox, where it cannot be approved. Other advisories show a warning and are stored with the inbox item. A network failure does not block installs. Tether uses `curl` with a 10-second limit. For a package without a pinned version, Tether finds the release that would install: the newest stable release older than `packages.min_release_age_days`, from the npm registry, PyPI or RubyGems. For npm and pnpm, Tether asks the manager for its registry, `@scope:registry` included, and reads the release from there. A registry that needs a login, or a bun registry setting, makes the release unknown. Tether reads PyPI version names as uv does, so `1.0-post1` is the stable release `1.0.post1`. It skips a PyPI release whose `requires_python` excludes the Python that `uv python find` reports. It does not check wheel platform tags. OSV checks that release, and Tether installs it pinned, so the release OSV checked is the release that installs. A `MAL-` advisory for an older release no longer blocks the package. When Tether cannot find the release, OSV checks all releases and only `MAL-` advisories count. The package then waits in the inbox as "malicious releases, install version unknown". Unlike a malicious release that would install, you can approve it, after a warning. The dashboard and `tether packages approve` ask about it as about an OSV outage
- The dashboard has a new Security tab for the approval inbox. It shows each held package with its reasons, source machine, commit and OSV advisories, and each machine key that waits for trust with its fingerprint. A changed key shows a loud warning. Press `a` to approve and install or to trust a key, `x` to reject, or `A` to approve all packages that are not malicious. The tab also lists the trusted machines. The header shows the number of held items. The approve-all question lists each package with its version, manager and tap. Scroll the list with `j`, `k`, the arrow keys or Page Up and Page Down. Approve all approves only the packages its question listed. `a` and `x` act on the item as the screen last showed it. If a refresh changed it since then, for example its version or key, Tether refuses and shows the new item first. Ctrl+K lists held items too, but it only opens them on the Security tab, so you see the fingerprint and warnings before you decide. The dashboard layout has more room: a blank row under the tabs, padded panels, rows that shorten with an ellipsis instead of overlapping, larger machine cards with id, profile, packages and key trust, and a daemon log that shows a repeated line once with its count. The Config tab can edit the package security settings. The Config tab no longer shows the unused `sync_versions` settings. Tether still writes them as `false`, because 1.x builds fail to read a synced config.toml without them

### Changed

- `tether packages uninstall` is the new name of `tether packages remove`, which stays as an alias. Without a package, it opens the picker in a terminal. Before, bare `tether packages` opened an uninstall picker; now it lists
- `tether machines rename <NEW>` renames this machine. The form `rename <OLD> <NEW>` still works in 2.0 with a warning, and fails when OLD is not this machine
- `-y` now answers every prompt: it confirms an action and takes each question's default answer. Without a terminal and without `-y`, a prompt fails with a message that names `-y`. Before, most prompts failed with "The input device is not a TTY". `-y` does not approve inbox items, trust keys or pick an answer that has no safe default. The one exception is `tether packages approve --all -y`
- `tether upgrade` asks before it upgrades, and lists what it will change. Without a terminal it needs `-y`. It updates the trusted Homebrew taps before it makes the list, and the list shows casks whose version is `latest`. It then installs exactly the listed upgrades, so a release that passes the release-age limit meanwhile waits for the next upgrade. A manager that fails to list or upgrade its packages shows a warning, and the other managers still upgrade. The command then saves the upgrade time and exits with status 1. The report after an upgrade lists Homebrew formulae and casks whose version changed too
- Every command that fails now prints one `Error: ...` line on stderr and exits with status 1. A usage error, such as an unknown flag or a missing argument, exits with status 2, as before. Before, about 45 error paths printed an error and exited with 0, for example `tether config get` of an unknown key, `tether machines rename` of another machine and `tether identity unlock` without an identity
- Tether prints no colour codes when stdout is not a terminal or `NO_COLOR` is set
- `tether packages approve` and `reject` take the reviewed version, tap or key as `--expect`, and `tether machines trust` takes the fingerprint as `--fingerprint`. The positional forms still work. `tether packages approve machine:<id>` works as `tether machines trust <id>`
- A machine argument takes a machine id or a hostname that names exactly one machine. A hostname counts only from a trusted record or this machine's record, because anyone who can push can write a record's hostname. `tether machines trust` also takes the hostname of a new machine whose record a key signs. A name that matches no machine is an error. Before, `tether machines untrust` and `tether packages approve --all --from` with such a name exited with 0 and did nothing
- Package ids accept `brew:` and `cask:` for `brew_formulae:` and `brew_casks:`. `tether packages list` groups packages by the manager keys that ids use, and shows "this profile only" instead of `[]`
- `tether ignore secrets ...` and `tether ignore files ...` replace `tether ignore add|list|remove` and `tether ignore dotfile|project|sync-list|sync-remove`. The old forms still work
- `tether sync --dry-run` ends with "Dry run: nothing changed" instead of "Synced"
- Upgrade note: packages no longer cross profiles on their own. Before, every machine installed the packages of all machines, whatever their profile. After the upgrade, a machine in profile `server` stops installing packages that only `dev` machines list, and the other way round. Nothing is uninstalled. The first sync that finds such packages names them once, with the command that shares them. It does not name packages this machine removed. Tether does not change config.toml for you

- A package from the manifests no longer installs on its own unless a trusted machine record lists that exact package and version. A record is trusted when its signature verifies against the key this machine trusts for that machine id. The manifests and commit signatures do not count. Other packages wait in the approval inbox. Tether checks this again on every sync. It checks every held package again too, whatever held it. A package installs, and its inbox item goes, once no check holds it: for example when a record that lists it becomes trusted, npm can enforce the release-age limit, or its tap becomes trusted. When the reasons change, Tether reports the item again. Set `packages.auto_install_from_trusted = false` to hold packages from trusted records too. The manifests list package names only, as 1.x writes them. A sync installs the newest version that a trusted record lists, so nobody can pick a version through the manifest. A sync merges into the manifests: it adds the names that this machine's record and the records it trusts list, keeps the other lines because a line grants no trust, and removes a name only when no machine record in the repo lists that package. So machines that trust different records do not undo each other's manifests, and a machine that upgrades first keeps the packages of machines that have not synced since. A sync writes the manifests before it installs, so the first sync after you trust a machine installs its packages. `tether rollback` installs the newest trusted version too, unless you confirm the snapshot's version in a terminal
- Tether now checks `packages.min_release_age_days` itself for synced packages whose manager cannot: gem, npm before 11.10, pnpm before 10.16 and bun before 1.3. It installs the exact release it checked in the registry: the newest release published at least that many days ago, or the version a trusted record lists when its publish time is old enough. It reads the publish time from `created_at` on RubyGems and from `time` on the npm registry. A trusted version that is too new waits in the inbox as "newer than the release-age limit" until it is old enough. Only when the registry check fails does the package wait as "release age not checked". Before, these packages installed after a warning. Tether asks the registry about a package once per day, and not at all for a package that waits for trust anyway
- Homebrew taps that are not trusted, and formulae and casks from them, now wait in the approval inbox. Before, Tether skipped them with a warning. A sync also taps a new tap only when a trusted machine record lists it, as for packages. Other taps wait in the inbox, even trusted ones. A tap that this machine has tapped already never waits and is not tapped again
- The dashboard has a new look. It uses Catppuccin Mocha or Latte colors when the terminal supports true color, and it picks one from the terminal background. Set `dashboard.theme` to `mocha`, `latte`, `ansi` or `auto` to choose, for example with `tether config set dashboard.theme mocha`. Terminals without true color keep the 16-color theme
- The dashboard header shows the machine, the daemon state and a spinner while a sync runs. The Overview tab shows a chart of sync commits per day
- The Machines tab shows a card for each machine with its last sync, online/idle/stale state, OS and tether version. A machine is online when its record is at most 90 minutes old, because an idle machine updates its record once an hour
- File and manifest diffs in the dashboard show line numbers and colored added and removed lines
- Dashboard messages appear as notices in the top-right corner and disappear after a few seconds. Package warnings from background work, such as an incomplete OSV check, appear there too and go to the log, instead of printing over the screen
- The dashboard supports the mouse: click a tab, a row or a key hint, and scroll lists with the wheel
- Press Ctrl+K in the dashboard to search actions, tabs, files and packages
- The dashboard redraws only when something changes, so it uses almost no CPU when idle
- In the package import list, the "Install?" question now shows above the list. Before, the list hid it

### Security

- Dashboard installs now check OSV first. A `MAL-` advisory blocks them, and so does a package the inbox holds as malicious. When OSV cannot be reached, the dashboard asks before it installs, and only `y` installs. Approvals on the Security tab, one or all, ask the same question before they approve, and the question names each package OSV could not check. These questions, and the question that removes an old record, can open while you type elsewhere. They ignore `y` until 0.4 seconds after they appear, and show dim buttons with a countdown until then. `n`, Esc and Enter cancel them at once. `tether packages approve` checks OSV before it approves too. When OSV cannot be reached, it asks in a terminal, and without a terminal it fails and approves nothing. Rollbacks get the checks of `tether rollback`
- When a trusted machine signs its record with a different key, Tether shows a warning and asks through the inbox. Until you approve the new key, Tether does not trust that record. When a trusted machine's record fails its signature, for example after someone edits it in the repo or deletes its `.sig` file, Tether ignores the record and warns: "Record for <id> fails its signature". It warns once for each version of the record, and the daemon also logs it and sends one notification. Tether records each warning in `~/.tether/state.json`. A held package from such a record shows "signature failed", in red on the Security tab. Approve all and "Install all" leave it held. To approve it, `tether packages approve` asks you to type its version and then asks again, and the dashboard asks a second question
- Trust never moves to another machine id on its own. A renamed machine waits in the inbox as a new machine, even with a key that this machine trusts under the old name. `tether machines rename` now renames only the machine it runs on. It builds the record from `~/.tether/machine.json` and signs it again under the new id, under the sync lock. It refuses to rename another machine, because only that machine can sign its record
- A machine record in `machines/` must have a valid machine id (letters, digits, `.`, `_` and `-`) as its file name, and the same id inside it. Tether ignores other records and shows a warning. Tether counts its own record only when this machine's key signed it. A signed record that has entries Tether drops, such as a version range, does not count as signed
- `tether machines remove`, `tether machines rename` and `tether diff` now refuse a machine id that is not a plain name. Before, `tether machines remove ../../state` deleted `~/.tether/state.json`, outside the sync repo. Both removals also refuse a record file that is a symlink or resolves outside the sync repo. Machine ids are case-sensitive: on a case-insensitive file system, `tether machines remove MAC` deleted `machines/mac.json` but left the key of `mac` trusted. It now refuses and names the exact id. Like the dashboard, `tether machines remove` removes only the record it asked about. If a sync changed the record while the question was open, it refuses under the sync lock. When a failed commit puts the record back, Tether creates each file again and never writes through a symlink. `tether -y machines remove <id>` removes without asking, also without a terminal. It still removes only the record it read when it started, and prints that record's hostname, last sync and package count
- This machine now builds its record from its own copy in `~/.tether/machine.json`, not from the copy in the sync repo. Anyone who can push could add removals or ignored files to the repo copy, and this machine would then keep and sign them. On the first sync after an upgrade, Tether reads the repo copy once, if this machine signed it or an earlier build wrote it without a signature. After this machine saves its first signed record, Tether never reads an unsigned repo copy as its own. If `~/.tether/machine.json` is missing, Tether reads the repo copy only when this machine's key signed it and its generation is not older than the last one this machine saved. The dashboard shows this machine's live package list but no longer writes it to the record. Only a sync writes the package names, and it reads them together with their versions
- Each machine record now has a `generation` that grows with every save, and the signature covers it. Tether keeps the newest generation it has accepted from each machine key in `~/.tether/record_generations.json`. An older signed record from the same key, such as one restored from git history, no longer counts and shows a warning. A different record with the generation already accepted does not count either. Every save of this machine's record, from a sync, `tether ignore` or a dashboard install, holds the sync lock and counts on from the newest saved generation, so two saves never share a generation. Approving a machine's new key still replaces its old key
- Tether now checks every package name from a manifest before it runs a package manager. It skips names that look like flags, URLs, paths, tarballs or `git+`/`github:`/`file:`/`link:` specs, and shows a warning
- New setting `packages.min_release_age_days` (default 7, 0 turns it off). npm, pnpm, bun and uv installs and upgrades skip releases newer than this. The daemon does not auto-upgrade a manager that is too old to enforce it (npm before 11.10, pnpm before 10.16, bun before 1.3, and gem), and it logs a warning once
- npm, pnpm and bun now install and upgrade with install scripts turned off. List packages that need their scripts in `packages.allow_scripts`
- Homebrew taps outside `homebrew/*` must be listed in `packages.brew.trusted_taps`. Tether skips other taps, and formulae and casks from them, and shows a warning
- Tether now rejects package names and versions that a package manager reads as a local file. The check ignores case. It covers `.tgz`, `.tar` and `.tar.gz` for npm, pnpm and bun, `.gem` for gem, `.rb`, `.json` and bottle tarballs for Homebrew, and `.tar.gz`, `.whl` and `.zip` for uv
- Tether now runs every package manager in the empty directory `~/.tether/run`. Before, a package manager ran in your current directory and could install a local file or read project config from it. `gem install` and `gem update` also use `--remote`, so gem never installs a `*.gem` file
- pnpm 11 and later now get `--config.minimum-release-age-strict=true` with the release-age cutoff. Without it, pnpm 11.0 to 12.2 could install a too-new version and add it to `minimumReleaseAgeExclude`
- `packages.allow_scripts` now runs scripts only for the listed package. pnpm 10.4 and later install it with `--allow-build=<name>`, and npm 12 and later with `--allow-scripts=<name>`. On older npm or pnpm the listed package also installs with scripts off, because those versions would run the scripts of all its dependencies too. Tether shows a warning once
- uv now gets the release-age limit as a duration (`--exclude-newer "7 days"`) on uv 0.9.17 and later. uv saves the limit in each tool receipt. A saved timestamp kept later upgrades at that date, but a saved duration stays relative. With `packages.min_release_age_days = 0`, uv 0.11.24 and later get `--exclude-newer false`, which clears a saved limit. On older uv, run `uv tool install --force <name>` to clear it
- Tether now looks up the tap of a short Homebrew name, such as `bun`, before it installs it. brew can resolve a short name to any tapped repository, so a name from an untrusted tap is skipped like a qualified one. The lookup asks brew only about the core tap and reads other taps' file names, because `brew info` runs a formula's Ruby code
- `tether upgrade` now asks before it upgrades a manager that cannot enforce `packages.min_release_age_days`, such as gem or an old npm. Without a terminal, it skips that manager
- Homebrew upgrades now upgrade only outdated formulae and casks from trusted taps. Before, Tether ran a plain `brew upgrade`, which also upgraded packages from untrusted taps. Tether reads the tap of each installed package from its install receipt and names only trusted packages to brew, because brew runs the Ruby code of each package it loads. Tether no longer runs `brew update`, because it loads every installed package after it fetches the taps. Tether updates trusted taps with `git pull --ff-only` before it lists the upgrades, and brew refreshes its own data for the core taps. Run `brew update` yourself to update Homebrew
- npm, pnpm, bun, uv and gem upgrades, from the daemon or `tether upgrade`, now check each target version against OSV first. A package whose target has a `MAL-` advisory keeps its installed version and waits in the inbox as "malicious upgrade". The hold blocks only that target version. The next upgrade drops the hold when that version is no longer the target, for example when a clean later release replaces it. Tether reads the targets from `npm outdated -g --json`, `pnpm outdated -g --format json`, the `bun outdated -g` table, `uv tool list --outdated` and `gem outdated`. Homebrew has no OSV data, so brew upgrades are not checked
- `tether rollback` now lists each package and version it would install and asks before it changes anything. Before, it ran a full sync first, so a declined rollback still left that sync's installs and dotfile changes. It no longer syncs before it asks. It records each package it removes in this machine's record, so the next sync does not install it again. Without a terminal it needs `-y`. A snapshot version other than the newest trusted one installs only when you confirm that version in a terminal. `--yes` does not confirm it. It then checks the packages it would install like a sync does, and a version you confirmed counts as approved. Packages that fail a check wait in the inbox, OSV included. It holds the sync lock for the whole rollback, so the daemon cannot sync between its steps. The dashboard relies on these checks: it runs `tether rollback --yes` after its own question and no longer checks OSV itself
- A dashboard restore from a backup now backs up the current file first, as a restore from a commit does. It writes the file atomically, and it waits until no tether command runs
- When a sync ignores a machine's record as replayed or failing its signature, the dashboard trust question shows a red warning. This machine's card shows the fingerprint of its own key file
- `tether packages approve` now takes the sync lock while it installs, so the daemon cannot install the same package at the same time. `tether upgrade` takes it too, because it queues malicious upgrade targets in the inbox

### Fixed

- `tether upgrade` and the daemon's daily update could downgrade a package. With `packages.min_release_age_days`, npm, pnpm, bun and uv pick the newest release old enough, which can be older than the installed one. Each upgrade now installs only targets newer than the installed version, at that exact version, and says which packages stay. npm failed its whole upgrade when one global package had no release older than the limit, such as corepack ("No versions available for corepack"). Tether now skips that package with a one-line note, and the other npm packages upgrade. A package whose own check fails is skipped with a warning too. Each ecosystem orders its own versions: PEP 440 for uv, so `1.0.dev1` is older than `1.0a1` and `1!2.0` is newer than `3.0`, semver for npm, pnpm and bun, and RubyGems rules for gem. When gem cannot list its outdated gems, Tether skips gem with a warning. Before, it ran `gem update` without names, which updated every gem without the OSV check. pnpm, bun and uv leave such a package out of their outdated check, so it stays. After an upgrade, `tether upgrade` reads the installed versions again and lists only the packages whose version changed
- `tether config set` now refuses a key that the config does not have, such as `sync.nope`. Before, it saved the key and the config ignored it
- `tether config edit` without a terminal now fails. Before, it started an editor that waited for input
- Tether now writes config.toml maps, such as `machine_profiles` and `profiles`, in sorted order. Before, each save could order them differently, so machines saw a change where there was none and pushed config.toml back and forth
- A machine that takes a remote config.toml now records its hash. Before, the next remote change looked like a local edit, and the machine pushed its older copy over it. So a change such as a new profile could get lost
- `tether config set` now reads the value as a TOML value, so arrays work, for example `tether config set packages.brew.trusted_taps '["azure/kubelogin"]'`. A bare word that is not TOML, such as `mocha`, stays a string. An error now exits with a non-zero status. Before, an array failed to parse and the command still exited with 0
- `tether machines remove` and the dashboard removal now keep the record when the commit fails, for example on a stale index lock. Before, the files stayed deleted and a retry reported that the machine was not found
- A uv tool that Tether installed at a pinned version now upgrades again. uv saves `name==version` in the tool receipt, so Tether installs the bare name a second time to drop that pin and keep the installed version. The second install runs offline, so uv cannot fetch a release other than the one OSV checked. A pin that you set, such as `uv tool install name==1.0`, stays. `tether upgrade` shows that tool as "pinned at 1.0, not upgraded". Before, it listed the tool as upgraded although uv kept it at the pin
- A pnpm package that Tether installed at a pinned version now upgrades again. pnpm saves the exact version as the range, so upgrades now add the exact target version that `pnpm outdated --latest` reports under the release-age limit. pnpm upgrades can now cross major versions, like npm upgrades of global packages
- Tether no longer installs Homebrew casks on Linux, and it honours `packages.brew.sync_casks = false`. Before, a Linux machine tried each cask from a Mac on every sync, and could hold it in the inbox as "untrusted tap". A cask that this machine does not install no longer brings its tap into this machine's profile, so Tether does not tap it or hold it in the inbox
- A synced package that fails to install now waits 24 hours before Tether tries it again, or until its version changes. Tether warns once, not on every sync. `tether packages list` and the dashboard Packages tab show failed installs. The list stays on this machine, in `~/.tether/state.json`
- A failed `brew bundle` no longer counts as a Homebrew install
- Machine records now name their OS family (`os`, such as `macos` or `linux`), and the signature covers it. A record from an earlier build has no `os`: Tether reads it as macOS when its OS version starts with "macOS", and as Linux otherwise. A record with no OS data counts as this machine's OS. When only machines on another OS list the newest trusted version of a package, Tether tries that version first. If it fails, Tether finds the newest release that suits this machine, with the release-age limit of an unpinned line. No trusted record lists that release, so it waits in the inbox as "another OS pins a version that fails here", with its OSV advisories. Approve it to install exactly that release. Approval checks OSV again and fails when OSV cannot be reached, as for other items. A malicious release waits as malicious, and a rejected release or one the inbox holds as malicious is not offered. For example, a uv tool from a Mac that needs a newer Python can install on Linux after you approve it
- gem now picks the newest release whose `required_ruby_version` admits the local Ruby (`ruby -e 'print RUBY_VERSION'`). Before, it picked the newest release, which could fail to install
- The gem list no longer records gems that ship with Ruby, such as `rake`, `minitest` and `net-imap`, or Debian's packaged gems. A newer version that you installed still counts. On a Ruby built by ruby-build or rbenv, Tether cannot tell these gems apart and still lists them
- A machine without `opendiff`, such as a Linux machine, now uses `vimdiff` when the synced merge tool is `opendiff`. Before, the merge failed
- On Linux, desktop notifications now use `notify-send`. Without it, Tether logs this once and shows no notifications
- The daemon now writes info lines, such as each sync, to `daemon.log`. Before, it wrote only errors unless `RUST_LOG` was set. The daemon also logs when its signal handlers are ready and when a sync that SIGHUP started ends
- In the dashboard, an uninstall now waits for a running sync, and the header shows that it waits. When the profile cannot leave the package, the dashboard shows an error and starts no sync. When the dashboard cannot read the package profiles, it refuses to uninstall and shows why
- The dashboard now removes a Config list item by its value, from the config on disk. Before, it removed by position, so a reload could remove another item
- Ctrl and Alt keys in the dashboard no longer act as plain letters. Config lists other than Dotfiles no longer show `t`
- An idle machine no longer makes a commit on every daemon sync. Tether saves and signs this machine's record only when something other than its last sync time changed, or once an hour so other machines see that it runs. Records now list their keys in a fixed order

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
