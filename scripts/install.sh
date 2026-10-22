#!/bin/sh
# Install DevCat from the terminal (macOS, Linux):
#
#   curl -fsSL https://devcathq.github.io/DevCat/install.sh | sh              # the CLI (devcat serve)
#   curl -fsSL https://devcathq.github.io/DevCat/install.sh | sh -s -- --app  # the desktop app
#
# Options: --app (the app instead of the CLI), --both, --version vX.Y.Z (default: latest),
# --dir <dir> (where the CLI goes; default ~/.local/bin, or /usr/local/bin as root).
# Nothing is installed system-wide unless you run it as root. Windows: see install.ps1.
set -eu

REPO="devcathq/DevCat"
want_cli=1
want_app=0
version=""
dir=""

while [ $# -gt 0 ]; do
  case "$1" in
    --app) want_cli=0; want_app=1 ;;
    --both) want_cli=1; want_app=1 ;;
    --version) version="${2:?--version needs a value}"; shift ;;
    --version=*) version="${1#*=}" ;;
    --dir) dir="${2:?--dir needs a value}"; shift ;;
    --dir=*) dir="${1#*=}" ;;
    -h|--help) echo "Usage: install.sh [--app | --both] [--version vX.Y.Z] [--dir <dir>]"; exit 0 ;;
    *) echo "Unknown option: $1" >&2; exit 2 ;;
  esac
  shift
done

say() { printf '%s\n' "$*"; }
fail() { printf 'devcat install: %s\n' "$*" >&2; exit 1; }

if [ -n "$version" ]; then
  case "$version" in v*) ;; *) version="v$version" ;; esac
  base="https://github.com/$REPO/releases/download/$version"
else
  base="https://github.com/$REPO/releases/latest/download"
fi
# Testing: a local copy of the release files.
base="${DEVCAT_DOWNLOAD_BASE:-$base}"

# download <url> <file>
if command -v curl > /dev/null 2>&1; then
  download() { curl -fsSL --retry 3 -o "$2" "$1"; }
elif command -v wget > /dev/null 2>&1; then
  download() { wget -q -O "$2" "$1"; }
else
  fail "needs curl or wget"
fi

sha256() {
  if command -v sha256sum > /dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1
  else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

os=$(uname -s)
arch=$(uname -m)
case "$arch" in
  arm64|aarch64) arch=aarch64 ;;
  x86_64|amd64) arch=x86_64 ;;
  *) fail "no DevCat build for $arch" ;;
esac
case "$os" in
  Darwin) target="$arch-apple-darwin" ;;
  Linux) target="$arch-unknown-linux-gnu" ;;
  *) fail "this script is for macOS and Linux; on Windows use install.ps1" ;;
esac

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM

on_path() { case ":$PATH:" in *":$1:"*) return 0 ;; *) return 1 ;; esac; }

install_cli() {
  if [ -z "$dir" ]; then
    if [ "$(id -u)" = 0 ]; then dir=/usr/local/bin; else dir="$HOME/.local/bin"; fi
  fi
  file="devcat-cli-$target.tar.gz"
  say "Downloading ${file}..."
  download "$base/$file" "$tmp/$file" || fail "couldn't download $base/$file"
  download "$base/$file.sha256" "$tmp/$file.sha256" || fail "couldn't download the checksum"
  [ "$(sha256 "$tmp/$file")" = "$(cut -d' ' -f1 < "$tmp/$file.sha256")" ] || fail "checksum mismatch for $file"
  tar -xzf "$tmp/$file" -C "$tmp"
  mkdir -p "$dir"
  mv -f "$tmp/devcat" "$dir/devcat"
  chmod 755 "$dir/devcat"
  say "Installed $("$dir/devcat" version) to $dir/devcat"
  if [ "$os" = Linux ] && command -v ldconfig > /dev/null 2>&1 && ! ldconfig -p 2>/dev/null | grep -q 'libdbus-1\.so\.3'; then
    say "Note: DevCat needs libdbus (Debian/Ubuntu: sudo apt install libdbus-1-3; Fedora: sudo dnf install dbus-libs)."
  fi
  on_path "$dir" || say "Add $dir to your PATH, e.g.: echo 'export PATH=\"$dir:\$PATH\"' >> ~/.profile"
  say "Start it with: devcat serve   (devcat help for more)"
}

install_app() {
  case "$os" in
    Darwin)
      a=$([ "$arch" = aarch64 ] && echo aarch64 || echo x64)
      file="DevCat_$a.app.tar.gz"
      say "Downloading ${file}..."
      download "$base/$file" "$tmp/$file" || fail "couldn't download $base/$file"
      tar -xzf "$tmp/$file" -C "$tmp"
      apps=/Applications
      [ -w "$apps" ] || { apps="$HOME/Applications"; mkdir -p "$apps"; }
      if pgrep -xq devcat 2> /dev/null; then say "DevCat is running: quit it to use the new version."; fi
      rm -rf "$apps/DevCat.app"
      mv "$tmp/DevCat.app" "$apps/DevCat.app"
      say "Installed DevCat to $apps/DevCat.app"
      ;;
    Linux)
      [ "$arch" = x86_64 ] || fail "the desktop app is built for x86_64 Linux only; the CLI works here (drop --app)"
      download "$base/latest.json" "$tmp/latest.json" || fail "couldn't read the release"
      v=$(sed -n 's/.*"version": *"\([^"]*\)".*/\1/p' "$tmp/latest.json" | head -1)
      [ -n "$v" ] || fail "couldn't read the version"
      file="DevCat_${v#v}_amd64.AppImage"
      say "Downloading ${file}..."
      download "$base/$file" "$tmp/$file" || fail "couldn't download $base/$file"
      d="$HOME/.local/bin"
      mkdir -p "$d"
      mv -f "$tmp/$file" "$d/DevCat.AppImage"
      chmod 755 "$d/DevCat.AppImage"
      apps="$HOME/.local/share/applications"
      mkdir -p "$apps"
      printf '[Desktop Entry]\nType=Application\nName=DevCat\nExec=%s %%F\nTerminal=false\nCategories=Development;\n' "$d/DevCat.AppImage" > "$apps/devcat.desktop"
      say "Installed DevCat $v to $d/DevCat.AppImage (it updates itself). The .deb / .rpm are on the releases page."
      ;;
  esac
}

[ "$want_cli" = 1 ] && install_cli
[ "$want_app" = 1 ] && install_app
exit 0
