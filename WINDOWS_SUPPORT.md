# Windows Support

## What works

- **Dotfile sync** — Push/pull encrypted dotfiles, `.gitconfig` syncs via `~/.gitconfig` (`%USERPROFILE%\.gitconfig`)
- **Package management** — WinGet integration: list, install, uninstall, upgrade, export/import manifests
- **Daemon** — Background sync via Task Scheduler (`schtasks`), RestartOnFailure for auto-restart
- **Project configs** — Sync project-level configs with symlink support (falls back to copy without Developer Mode)
- **Team sync** — Symlinks for team configs, copy fallback on Windows
- **Notifications** — PowerShell toast notifications for sync conflicts
- **CI** — `ubuntu-latest` + `macos-latest` + `windows-latest` matrix
- **Release** — Signed macOS binaries + Windows `.zip` with SHA256

## Platform behavior differences

| Feature | macOS | Windows |
|---------|-------|---------|
| Daemon install | launchd (KeepAlive) | Task Scheduler (RestartOnFailure) |
| Symlinks | Native | Requires Developer Mode; copies as fallback |
| Notifications | AppleScript | PowerShell toast |
| Default merge tool | `opendiff` | `code` (VS Code) |
| Default editor | `nano` | `notepad` |
| File permissions | `0o600` for secrets | ACL restricted via `icacls` |
| Process management | `kill`/signals (graceful SIGTERM) | `tasklist`/`taskkill /F` (no graceful signal for detached processes) |
| Package manager | Homebrew | WinGet |

## Architecture notes

- Package managers check `is_available()` at runtime — brew returns false on Windows, winget returns false on macOS
- Default dotfiles (`.zshrc`, `.bashrc`) have `create_if_missing: false` so they won't be created on Windows
- Config dir is `~/.tether/` → `C:\Users\<name>\.tether\` via the `home` crate
- State keys use forward slashes on all platforms (normalized with `.replace('\\', "/")`)
- Path validation rejects `..`, `/`, `\`, and drive letters (`C:\`) for traversal safety
- `create_symlink()` in `sync/mod.rs` handles the Developer Mode fallback centrally

## Known limitations

- No ARM64 Windows build (x86_64 runs under emulation)
- `winget list` parser uses fixed-width column positions — handles CJK double-width characters but may break with non-English locales (different header text)
- No WinGet manifest in release pipeline (users install from GitHub releases)
