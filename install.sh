#!/bin/sh
# slick installer for Linux.
#
#   curl -fsSL https://slick.leonavas.com/install.sh | sh
#
# Downloads the latest release (or SLICK_VERSION=v0.2.0) for this machine,
# checks its SHA-256 and puts `slick` in ${SLICK_INSTALL_DIR:-$HOME/.local/bin}.
# No sudo. A binary there updates itself from the app.
#
# This file is the source; landing/public/install.sh is an identical copy
# served by the site. Edit this one, then copy it over.

set -eu

REPO="leonavas/slick"

say() { printf 'slick: %s\n' "$*"; }
err() { printf 'slick: error: %s\n' "$*" >&2; exit 1; }
has() { command -v "$1" >/dev/null 2>&1; }

# fetch URL FILE, or fetch URL to stdout
fetch() {
    if has curl; then
        if [ $# -gt 1 ]; then curl -fsSL --proto '=https' -o "$2" "$1"; else curl -fsSL --proto '=https' "$1"; fi
    elif has wget; then
        if [ $# -gt 1 ]; then wget -q -O "$2" "$1"; else wget -q -O - "$1"; fi
    else
        err "needs curl or wget"
    fi
}

sha256() {
    if has sha256sum; then
        sha256sum "$1" | cut -d' ' -f1
    elif has shasum; then
        shasum -a 256 "$1" | cut -d' ' -f1
    else
        err "needs sha256sum or shasum to check the download"
    fi
}

target() {
    os=$(uname -s)
    arch=$(uname -m)
    case "$os" in
        Linux) ;;
        Darwin) err "macOS is not supported yet; build from source: cargo install --locked --git https://github.com/$REPO" ;;
        *) err "unsupported OS: $os (on Windows use install.ps1)" ;;
    esac
    case "$arch" in
        x86_64 | amd64) echo "x86_64-unknown-linux-gnu" ;;
        aarch64 | arm64) echo "aarch64-unknown-linux-gnu" ;;
        *) err "unsupported architecture: $arch" ;;
    esac
}

latest() {
    fetch "https://api.github.com/repos/$REPO/releases/latest" |
        sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n 1
}

# The sign-in window is a WebKitGTK window: slick does not start without it.
# Only a warning, since the package name differs per distro.
check_webkit() {
    if has ldconfig && ldconfig -p 2>/dev/null | grep -q 'libwebkit2gtk-4.1'; then return; fi
    if has pkg-config && pkg-config --exists webkit2gtk-4.1 2>/dev/null; then return; fi
    say "warning: WebKitGTK 4.1 not found; slick needs it. Install it with one of:"
    say "  sudo apt install libwebkit2gtk-4.1-0     # Debian/Ubuntu"
    say "  sudo pacman -S webkit2gtk-4.1            # Arch"
    say "  sudo dnf install webkit2gtk4.1           # Fedora"
}

main() {
    tgt=$(target)
    dir="${SLICK_INSTALL_DIR:-$HOME/.local/bin}"

    tag="${SLICK_VERSION:-}"
    if [ -z "$tag" ]; then
        tag=$(latest) || true
        [ -n "$tag" ] || err "could not find the latest release on GitHub"
    fi
    case "$tag" in v*) ;; *) tag="v$tag" ;; esac
    ver="${tag#v}"

    name="slick-$ver-$tgt.tar.gz"
    url="https://github.com/$REPO/releases/download/$tag/$name"

    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT
    trap 'exit 1' INT TERM

    say "downloading slick $ver ($tgt)"
    fetch "$url" "$tmp/$name" || err "download failed: $url"
    fetch "$url.sha256" "$tmp/$name.sha256" || err "download failed: $url.sha256"

    want=$(cut -d' ' -f1 <"$tmp/$name.sha256")
    got=$(sha256 "$tmp/$name")
    [ "$want" = "$got" ] || err "SHA-256 mismatch ($got, expected $want)"

    tar -xzf "$tmp/$name" -C "$tmp" slick || err "could not unpack $name"
    mkdir -p "$dir"
    # Replaced by a rename, so a running slick keeps its own copy.
    cp "$tmp/slick" "$dir/.slick.new"
    chmod 755 "$dir/.slick.new"
    mv -f "$dir/.slick.new" "$dir/slick"
    say "installed $dir/slick"

    case ":${PATH:-}:" in
        *":$dir:"*) ;;
        *)
            say "warning: $dir is not on your PATH. Add it with:"
            say "  echo 'export PATH=\"$dir:\$PATH\"' >> ~/.profile   # then open a new shell"
            ;;
    esac

    check_webkit
    say "done. Run: slick"
}

main "$@"
