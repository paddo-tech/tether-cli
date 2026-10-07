#!/bin/sh
# Builds what the e2e suite needs and skips what is built already:
#   target/e2e/bin/tether-<label>        a Linux binary per version, built in Docker
#   tether-e2e-machine, tether-e2e-real  the machine images, tagged by a hash of tests/e2e/docker
# Usage: tests/e2e/images.sh [ref...]
# HEAD is always built, with uncommitted changes to tracked files. A ref v* is labelled by
# its name, any other ref by its commit, so a cached binary never stands in for a moved branch.
set -eu
root=$(git rev-parse --show-toplevel)
docker_dir=$root/tests/e2e/docker
out=$root/target/e2e
mkdir -p "$out/bin"

label() {
    case $1 in
    v[0-9]*) echo "$1" ;;
    *) echo "ref-$(git -C "$root" rev-parse --short=12 "$1^{commit}")" ;;
    esac
}

build() {
    [ -x "$out/bin/tether-$2" ] && return
    echo "e2e: building tether $2" >&2
    ctx=$(mktemp -d)
    git -C "$root" archive --format=tar -o "$ctx/src.tar" "$1"
    if ! docker build --progress=plain -f "$docker_dir/Dockerfile.build" \
        --output "type=local,dest=$ctx/out" "$ctx" >"$out/build-$2.log" 2>&1; then
        tail -40 "$out/build-$2.log" >&2
        echo "e2e: build of $2 failed, see $out/build-$2.log" >&2
        exit 1
    fi
    mv "$ctx/out/tether" "$out/bin/tether-$2"
    rm -rf "$ctx"
}

head=$(git -C "$root" stash create)
head=${head:-HEAD}
# Keyed by every top-level entry of the archive except the ones the build never reads, so a
# new build input such as build.rs or an include_str! file counts, and a test change does not
key=$(git -C "$root" ls-tree "$head" | grep -v -E "	(tests|website|fastlane|\.github|[^/]*\.md)\$" | git hash-object --stdin | cut -c1-12)
build "$head" "head-$key"
cp "$out/bin/tether-head-$key" "$out/bin/tether-head"
for f in "$out"/bin/tether-head-*; do
    [ "$f" = "$out/bin/tether-head-$key" ] || rm -f "$f"
done
for ref in "$@"; do
    build "$ref" "$(label "$ref")"
done

hash=$(find "$docker_dir" -type f | LC_ALL=C sort | xargs cat | git hash-object --stdin | cut -c1-12)
for name in machine real; do
    file=Dockerfile
    [ $name = real ] && file=Dockerfile.real
    if ! docker image inspect "tether-e2e-$name:$hash" >/dev/null 2>&1; then
        echo "e2e: building image tether-e2e-$name:$hash" >&2
        docker build -q -t "tether-e2e-$name:$hash" -f "$docker_dir/$file" "$docker_dir" >/dev/null
    fi
    docker tag "tether-e2e-$name:$hash" "tether-e2e-$name:latest"
done
