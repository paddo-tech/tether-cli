#!/bin/sh
# Install or upgrade Tether: curl -fsSL https://tether-cli.com/install.sh | sh
# TETHER_INSTALL_DIR sets the directory (default ~/.local/bin). TETHER_VERSION pins a release.
set -eu

fail() {
    echo "Error: $1" >&2
    exit 1
}

# Every step runs from main, which runs on the last line, so a cut-off download runs nothing
main() {
    repo="paddo-tech/tether-cli"
    dir="${TETHER_INSTALL_DIR:-$HOME/.local/bin}"
    version="${TETHER_VERSION:-latest}"

    case "$(uname -s)-$(uname -m)" in
        Linux-x86_64 | Linux-amd64) target=x86_64-unknown-linux-musl ;;
        Linux-aarch64 | Linux-arm64) target=aarch64-unknown-linux-musl ;;
        Darwin-arm64) target=aarch64-apple-darwin ;;
        # A shell under Rosetta reports x86_64 on Apple silicon
        Darwin-x86_64)
            if [ "$(sysctl -n hw.optional.arm64 2>/dev/null)" = 1 ]; then
                target=aarch64-apple-darwin
            else
                target=x86_64-apple-darwin
            fi
            ;;
        *) fail "Tether has no build for $(uname -s) $(uname -m)" ;;
    esac

    if [ "$version" = latest ]; then
        base="https://github.com/$repo/releases/latest/download"
    else
        base="https://github.com/$repo/releases/download/v${version#v}"
    fi
    asset="tether-$target.tar.gz"
    bin="$dir/tether"
    # A symlink points at another install, which the daemon may run
    [ ! -L "$bin" ] || fail "$bin is a symlink. Upgrade the install it points to, or remove the link"

    tmp=$(mktemp -d)
    stage=""
    trap 'rm -rf "$tmp"; [ -z "$stage" ] || rm -f "$stage"' EXIT
    curl -fsSL "$base/$asset" -o "$tmp/$asset" || fail "could not download $base/$asset"
    curl -fsSL "$base/$asset.sha256" -o "$tmp/$asset.sha256" || fail "could not download $base/$asset.sha256"

    expected=$(cut -d ' ' -f 1 "$tmp/$asset.sha256")
    if command -v sha256sum >/dev/null 2>&1; then
        actual=$(sha256sum "$tmp/$asset" | cut -d ' ' -f 1)
    else
        actual=$(shasum -a 256 "$tmp/$asset" | cut -d ' ' -f 1)
    fi
    [ "$expected" = "$actual" ] || fail "the checksum of $asset does not match"

    tar -xzf "$tmp/$asset" -C "$tmp"
    mkdir -p "$dir"
    # A rename replaces the binary in one step, also while the daemon runs it
    stage=$(mktemp "$dir/.tether.XXXXXX")
    cp "$tmp/tether" "$stage"
    chmod 755 "$stage"
    installed=$("$stage" --version) || fail "the new binary does not run in $dir"
    mv "$stage" "$bin"
    stage=""
    echo "Installed $installed to $bin"

    # A daemon exits when the binary it runs changes; systemd and launchd start it again
    supervised=""
    for service in "$HOME/.config/systemd/user/tether.service" "$HOME/Library/LaunchAgents/com.tether.daemon.plist"; do
        [ -f "$service" ] || continue
        supervised=1
        grep -qF "$bin" "$service" ||
            echo "Warning: the daemon runs another tether. Run '$bin daemon install' to switch it to this one"
    done
    if [ -z "$supervised" ] && [ -f "$HOME/.tether/daemon.pid" ]; then
        echo "The daemon stops on its next check. Start it again with 'tether daemon start'"
    fi

    case ":$PATH:" in
        *":$dir:"*)
            found=$(command -v tether || true)
            [ "$found" = "$bin" ] || echo "Warning: $found comes first on your PATH, not $bin"
            ;;
        *) echo "Add $dir to your PATH, then run 'tether init'" ;;
    esac
}

main "$@"
