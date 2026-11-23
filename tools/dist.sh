#!/usr/bin/env bash
# Release steps shared by the local release script (tools/release-local.sh) and the release
# workflow (.github/workflows/release.yml), so a local build packs exactly what CI ships.
# Everything lands in dist/ (gitignored). Runs under bash 3.2 too (macOS runners).
#
#   tools/dist.sh version                  # the version in Cargo.toml
#   tools/dist.sh check-static BIN         # fails unless BIN is a static ELF (no INTERP, no NEEDED)
#   tools/dist.sh docs BIN                 # dist/docs: man page (+ .gz) and shell completions
#   tools/dist.sh archive TARGET BIN       # dist/swarmy-<version>-<target>.tar.gz (.zip on Windows)
#   tools/dist.sh deb TARGET               # dist/swarmy_<version>-1_<arch>.deb  (cargo-deb)
#   tools/dist.sh rpm TARGET RPM_ARCH      # dist/swarmy-<version>-1.<arch>.rpm  (cargo-generate-rpm)
#   tools/dist.sh sums                     # dist/SHA256SUMS over the packages in dist/
#   tools/dist.sh notes TAG                # release notes: the CHANGELOG section, else the tag message
#   tools/dist.sh formula                  # dist/packaging/swarmy.rb from packaging/homebrew (needs SHA256SUMS)
#
# `docs` runs BIN (`swarmy man`, `swarmy completions <shell>`) in an isolated, made-up home, so it
# never looks at the real Claude Code dirs. `deb` and `rpm` take the binary cargo left in
# $CARGO_TARGET_DIR/<target>/release (or target/<target>/release) and the docs in dist/docs.
set -euo pipefail
cd "$(dirname "$0")/.."

DIST=dist
REPO_URL="https://github.com/koorchik/swarmy"

die() {
    echo "dist: $*" >&2
    exit 1
}

version() {
    sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1
}

sha256() {
    if command -v sha256sum >/dev/null; then
        sha256sum "$@"
    else
        shasum -a 256 "$@"
    fi
}

check_static() {
    local bin=$1
    [ -f "$bin" ] || die "no binary at $bin"
    command -v readelf >/dev/null || die "readelf is needed (binutils)"
    if readelf -lW "$bin" | grep -q 'INTERP'; then
        die "$bin asks for a dynamic loader (INTERP): not static"
    fi
    if readelf -dW "$bin" | grep -q 'NEEDED'; then
        die "$bin needs shared libraries: $(readelf -dW "$bin" | grep NEEDED | tr -s ' ')"
    fi
    echo "dist: $bin is static (no INTERP, no NEEDED)"
}

docs() {
    local bin=$1
    [ -x "$bin" ] || die "no runnable binary at $bin"
    local out="$DIST/docs" home="$DIST/.home"
    rm -rf "$out" "$home"
    mkdir -p "$out/completions" "$home"
    home="$(cd "$home" && pwd)"
    run() {
        env -u CLAUDE_CONFIG_DIR HOME="$home" USERPROFILE="$home" SWARMY_ISOLATED=1 \
            SWARMY_DATA_DIR="$home/data" "$bin" "$@"
    }
    run man >"$out/swarmy.1"
    gzip -9n -c "$out/swarmy.1" >"$out/swarmy.1.gz"
    run completions bash >"$out/completions/swarmy.bash"
    run completions zsh >"$out/completions/_swarmy"
    run completions fish >"$out/completions/swarmy.fish"
    run completions powershell >"$out/completions/_swarmy.ps1"
    run completions elvish >"$out/completions/swarmy.elv"
    rm -rf "$home"
    for f in "$out/swarmy.1" "$out"/completions/*; do
        [ -s "$f" ] || die "$f came out empty"
    done
    echo "dist: man page and completions in $out"
}

archive() {
    local target=$1 bin=$2 ver name stage exe=swarmy
    ver=$(version)
    name="swarmy-$ver-$target"
    [ -f "$bin" ] || die "no binary at $bin"
    [ -f "$DIST/docs/swarmy.1" ] || die "run 'tools/dist.sh docs BIN' first"
    case "$target" in *windows*) exe=swarmy.exe ;; esac
    stage="$DIST/.stage/$name"
    rm -rf "$stage"
    mkdir -p "$stage/man" "$stage/completions"
    cp "$bin" "$stage/$exe"
    chmod 755 "$stage/$exe"
    cp README.md CHANGELOG.md LICENSE-MIT LICENSE-APACHE "$stage/"
    cp "$DIST/docs/swarmy.1" "$stage/man/"
    cp "$DIST"/docs/completions/* "$stage/completions/"
    case "$target" in
    *windows*)
        rm -f "$DIST/$name.zip"
        if command -v 7z >/dev/null; then
            (cd "$DIST/.stage" && 7z a -tzip -bso0 -bsp0 "../$name.zip" "$name")
        elif command -v zip >/dev/null; then
            (cd "$DIST/.stage" && zip -qr "../$name.zip" "$name")
        else
            die "zip or 7z is needed"
        fi
        echo "dist: $DIST/$name.zip"
        ;;
    *)
        # Fixed owner and order: the archive depends on the files only.
        (cd "$DIST/.stage" && tar -cf - "$name" | gzip -9n >"../$name.tar.gz")
        echo "dist: $DIST/$name.tar.gz"
        ;;
    esac
    rm -rf "$stage"
}

target_dir() {
    echo "${CARGO_TARGET_DIR:-target}"
}

deb() {
    local target=$1
    command -v cargo-deb >/dev/null || die "cargo-deb is needed: cargo install --locked cargo-deb"
    [ -f "$DIST/docs/swarmy.1.gz" ] || die "run 'tools/dist.sh docs BIN' first"
    [ -f "$(target_dir)/$target/release/swarmy" ] || die "build $target first"
    cargo deb --no-build --no-strip --target "$target" --output "$DIST/" --quiet
}

rpm() {
    local target=$1 arch=$2
    command -v cargo-generate-rpm >/dev/null ||
        die "cargo-generate-rpm is needed: cargo install --locked cargo-generate-rpm"
    [ -f "$DIST/docs/swarmy.1.gz" ] || die "run 'tools/dist.sh docs BIN' first"
    [ -f "$(target_dir)/$target/release/swarmy" ] || die "build $target first"
    cargo generate-rpm --target "$target" --target-dir "$(target_dir)" --arch "$arch" \
        --output "$DIST/swarmy-$(version)-1.$arch.rpm"
    echo "dist: $DIST/swarmy-$(version)-1.$arch.rpm"
}

sums() {
    (
        cd "$DIST"
        set --
        for f in *.tar.gz *.zip *.deb *.rpm; do
            if [ -f "$f" ]; then set -- "$@" "$f"; fi
        done
        [ $# -gt 0 ] || die "no packages in $DIST"
        sha256 "$@" >SHA256SUMS
    )
    cat "$DIST/SHA256SUMS"
}

notes() {
    local tag=$1 ver=${1#v} section
    # The `## v<version>` section of CHANGELOG.md, up to the next `## ` heading.
    section=$(awk -v h="## v$ver" '
        $0 == h || index($0, h " ") == 1 { on = 1; next }
        on && /^## / { exit }
        on { print }' CHANGELOG.md | sed -e '/./,$!d')
    if [ -z "$section" ]; then
        section=$(git tag -l --format='%(contents)' "$tag" 2>/dev/null || true)
    fi
    [ -n "$section" ] || section="swarmy $ver"
    printf '%s\n' "$section"
    cat <<EOF

Static binaries for Linux (x86_64, aarch64, ARMv7, ARMv6; musl), Windows (x86_64) and macOS
(universal). \`.deb\` and \`.rpm\` packages wrap the same Linux binaries. Check downloads against
\`SHA256SUMS\`.
EOF
}

sum_of() {
    local file=$1 s
    s=$(awk -v f="$file" '$2 == f || $2 == "*" f { print $1 }' "$DIST/SHA256SUMS")
    [ -n "$s" ] || die "$file is not in $DIST/SHA256SUMS"
    echo "$s"
}

formula() {
    local ver out
    ver=$(version)
    [ -f "$DIST/SHA256SUMS" ] || die "run 'tools/dist.sh sums' first"
    mkdir -p "$DIST/packaging"
    out="$DIST/packaging/swarmy.rb"
    sed -e "s|@VERSION@|$ver|g" \
        -e "s|@REPO@|$REPO_URL|g" \
        -e "s|@SHA256_MACOS@|$(sum_of "swarmy-$ver-universal-apple-darwin.tar.gz")|" \
        -e "s|@SHA256_LINUX_X86_64@|$(sum_of "swarmy-$ver-x86_64-unknown-linux-musl.tar.gz")|" \
        -e "s|@SHA256_LINUX_AARCH64@|$(sum_of "swarmy-$ver-aarch64-unknown-linux-musl.tar.gz")|" \
        packaging/homebrew/swarmy.rb >"$out"
    if grep -q '@[A-Z0-9_]*@' "$out"; then
        die "placeholders left in $out"
    fi
    echo "dist: $out"
}

cmd=${1:-}
[ -n "$cmd" ] && shift
case "$cmd" in
version) version ;;
check-static) check_static "$@" ;;
docs) docs "$@" ;;
archive) archive "$@" ;;
deb) deb "$@" ;;
rpm) rpm "$@" ;;
sums) sums ;;
notes) notes "$@" ;;
formula) formula ;;
*) sed -n '2,22p' "$0" >&2; exit 2 ;;
esac
