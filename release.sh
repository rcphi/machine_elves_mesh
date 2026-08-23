#!/usr/bin/env bash
#
# Builds everything a volunteer machine needs, into dist/.
#
#   ./release.sh
#
# Volunteer machines never compile anything. They receive binaries and compiled
# jobs, which is also how the real system works: a player's computer runs
# WebAssembly it was given, and only job authors need a compiler.
set -euo pipefail

# rustup installs outside the default PATH, and a non-interactive shell does not
# read the profile that adds it.
[[ -f "$HOME/.cargo/env" ]] && source "$HOME/.cargo/env"
command -v cargo >/dev/null || { echo "cargo not found — install Rust via rustup" >&2; exit 1; }

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DIST="$ROOT/dist"

# Fully static, so the binary does not care what C library the target machine
# has. Volunteer boxes are prepared centrally today and may not be tomorrow, and
# a binary that only runs on the distribution it was built on is a trap.
STATIC_TARGET="x86_64-unknown-linux-musl"

say() { printf '\n\033[1m==> %s\033[0m\n' "$*"; }

# Building the node compiles several hundred crates, and running one rustc per
# core will exhaust a small machine's memory before it exhausts its patience -
# a laptop can lock up entirely rather than fail. Left alone where there is
# room; capped where there is not.
if [[ -z "${CARGO_BUILD_JOBS:-}" ]]; then
    kb=$(awk '/MemTotal/ {print $2}' /proc/meminfo 2>/dev/null || echo 0)
    if (( kb > 0 && kb < 8000000 )); then
        export CARGO_BUILD_JOBS=2
        echo "note: under 8GB of memory, building with 2 jobs to avoid locking up"
    fi
fi

# This is a build machine's job. A machine that only *runs* the software needs
# none of it - that is what the static binaries are for.
if [[ "${1:-}" == "--help" ]]; then
    echo "release.sh - build binaries and jobs into dist/, for copying elsewhere"
    echo
    echo "Run this where a toolchain already lives. Machines that only run the"
    echo "software should receive dist/ and never build anything."
    exit 0
fi

build() {
    local crate="$1" binary="$2" target="" built=""

    # Add the target rather than silently falling back. Its absence is the
    # usual reason a static build "fails" on a machine that has everything else
    # it needs, and the fallback hid that behind a message about linking.
    if ! rustup target list --installed 2>/dev/null | grep -qx "$STATIC_TARGET"; then
        echo "  adding $STATIC_TARGET..."
        rustup target add "$STATIC_TARGET" >/dev/null 2>&1 || true
    fi

    if rustup target list --installed 2>/dev/null | grep -qx "$STATIC_TARGET"; then
        # Some dependencies compile C — the crypto under the transport layer
        # does — and need a C compiler targeting musl rather than this system.
        # Without it the build fails in a way that looks like the dependency
        # not supporting musl at all, which is what it was mistaken for.
        if command -v musl-gcc >/dev/null; then
            export CC_x86_64_unknown_linux_musl=musl-gcc
        fi
        # Errors are shown rather than discarded. Swallowing them turned every
        # cause - a missing target, a missing C compiler, running out of memory
        # - into the same unhelpful sentence.
        if ( cd "$ROOT/$crate" && cargo build --release --quiet --target "$STATIC_TARGET" ); then
            target="$STATIC_TARGET"
            built="$ROOT/$crate/target/$STATIC_TARGET/release/$binary"
        fi
    fi

    if [[ -z "$target" ]]; then
        # Falling back is better than failing, as long as the manifest says
        # which this is — a dynamically linked binary needs a matching machine.
        echo "  $binary: static build unavailable, linking against this system"
        command -v musl-gcc >/dev/null || echo "    (install musl-tools and it will probably succeed)"
        ( cd "$ROOT/$crate" && cargo build --release --quiet )
        target="$(rustc -vV | awk '/^host:/ {print $2}')"
        built="$ROOT/$crate/target/release/$binary"
    fi

    install -m 0755 "$built" "$DIST/$binary"
    printf '  %-12s %-30s %8s bytes  %s\n' "$binary" "$target" \
        "$(stat -c%s "$DIST/$binary")" \
        "$(file -b "$DIST/$binary" | grep -o 'statically linked\|dynamically linked')"
    echo "$binary $target" >> "$DIST/BUILT-WITH"
}

rm -rf "$DIST"
mkdir -p "$DIST/jobs"
: > "$DIST/BUILT-WITH"

say "Binaries"
build probe mesh-probe
build node  mesh-node

say "Jobs"
"$ROOT/jobs/build.sh" >/dev/null
for wasm in "$ROOT"/jobs/*/target/wasm32-unknown-unknown/release/*.wasm; do
    [[ -f "$wasm" ]] || continue
    install -m 0644 "$wasm" "$DIST/jobs/"
    printf '  %-24s %8s bytes\n' "$(basename "$wasm")" "$(stat -c%s "$wasm")"
done

say "Manifest"
{
    echo "built:    $(date -u +%Y-%m-%dT%H:%M:%SZ)"
    echo "commit:   $(git -C "$ROOT" rev-parse --short HEAD)$(git -C "$ROOT" diff --quiet || echo ' (with uncommitted changes)')"
    echo "rustc:    $(rustc --version)"
    cat "$DIST/BUILT-WITH"
} > "$DIST/VERSION"
rm -f "$DIST/BUILT-WITH"
cat "$DIST/VERSION" | sed 's/^/  /'

# Checksums travel with the files so a volunteer box can prove it received what
# was sent, and so two machines can confirm they are running the same thing —
# which matters, since identical jobs producing identical output is the property
# the whole design leans on.
( cd "$DIST" && find . -type f ! -name SHA256SUMS -print0 | sort -z | xargs -0 sha256sum > SHA256SUMS )

say "Archive"
TARBALL="$ROOT/machine-elves-$(git -C "$ROOT" rev-parse --short HEAD).tar.gz"
tar -czf "$TARBALL" -C "$ROOT" --transform 's,^dist,machine-elves,' dist
printf '  %s  (%s bytes)\n' "$(basename "$TARBALL")" "$(stat -c%s "$TARBALL")"

cat <<SUMMARY

Ready. To prepare a volunteer machine:

  scp $(basename "$TARBALL") user@box:
  ssh user@box 'tar -xzf $(basename "$TARBALL")'
  ssh user@box 'sudo ~/machine-elves/../machine_elves_mesh/provision/setup.sh --label NAME --dist ~/machine-elves'

or, if the repository is already on the box:

  sudo provision/setup.sh --label NAME --dist dist
SUMMARY
