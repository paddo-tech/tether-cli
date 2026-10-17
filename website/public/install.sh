#!/bin/sh
# Install or upgrade Tether: curl -fsSL https://tether-cli.com/install.sh | sh
# TETHER_INSTALL_DIR sets the directory (default ~/.local/bin). TETHER_VERSION pins a release.
set -eu

repo="paddo-tech/tether-cli"
dir="${TETHER_INSTALL_DIR:-$HOME/.local/bin}"
version="${TETHER_VERSION:-latest}"

fail() {
    echo "Error: $1" >&2
    exit 1
}

case "$(uname -s)-$(uname -m)" in
    Linux-x86_64 | Linux-amd64) target=x86_64-unknown-linux-musl ;;
    Linux-aarch64 | Linux-arm64) target=aarch64-unknown-linux-musl ;;
    Darwin-arm64) target=aarch64-apple-darwin ;;
    Darwin-x86_64) target=x86_64-apple-darwin ;;
    *) fail "Tether has no build for $(uname -s) $(uname -m)" ;;
esac

if [ "$version" = latest ]; then
    base="https://github.com/$repo/releases/latest/download"
else
    base="https://github.com/$repo/releases/download/v${version#v}"
fi
asset="tether-$target.tar.gz"

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
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
cp "$tmp/tether" "$dir/.tether.new"
chmod 755 "$dir/.tether.new"
mv "$dir/.tether.new" "$dir/tether"
echo "Installed $("$dir/tether" --version) to $dir/tether"

# macOS daemons copy the new binary by themselves; a systemd service runs the file in place
if command -v systemctl >/dev/null 2>&1 && systemctl --user is-active --quiet tether 2>/dev/null; then
    systemctl --user restart tether
    echo "Restarted the tether daemon"
fi

case ":$PATH:" in
    *":$dir:"*) ;;
    *) echo "Add $dir to your PATH, then run 'tether init'" ;;
esac
