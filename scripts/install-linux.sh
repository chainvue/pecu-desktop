#!/usr/bin/env bash
#
# Build Pecu and install it into a desktop.
#
#   scripts/install-linux.sh                     # into ~/.local, no root
#   scripts/install-linux.sh --prefix /usr/local # system-wide, needs root
#   scripts/install-linux.sh --uninstall         # takes it all back out
#
# ── What this is, and what it is not ─────────────────────────────────────────
#
# It is the Linux counterpart of `bundle.sh`, and it is a **different shape**
# on purpose. macOS wants one directory that is the application; Linux wants
# files in the places the desktop already looks — a binary on PATH, icons in
# the hicolor theme, and a `.desktop` entry that ties them together.
#
# ── Running it again ─────────────────────────────────────────────────────────
#
# Every file it writes is replaced: the binary, all eight icons, the `.desktop`
# entry. Nothing is merged and nothing is kept, so a re-install is the way to
# move to a newer build.
#
# **It does not touch the wallet.** The vault, the databases and the logs live
# in `$XDG_DATA_HOME/pecu`, or `~/.local/share/pecu` — a sibling of the
# `icons/` and `applications/` directories written here, and never opened by
# this script. `--uninstall` does not remove it either; a wallet is not a file
# an installer gets to delete.
#
# It is NOT a package. There is no `.deb`, no AppImage and no Flatpak here, so
# there is nothing to hand somebody else — this installs onto the machine that
# ran it. Packaging is its own decision; see `docs/LATER.md` §6.
#
# ── The link between the icon and the entry ──────────────────────────────────
#
# `Icon=pecu` in the `.desktop` file is a **name looked up in the icon theme**,
# not a path. It has to match the filenames under `hicolor/*/apps/`, which is
# why both come out of the same variable below and why
# `crates/pecu-app/tests/packaging.rs` asserts they agree. Get it wrong and the
# launcher shows a generic gear with no error anywhere.
#
# ── Untested ─────────────────────────────────────────────────────────────────
#
# **Nothing here has been run on Linux.** It was written on a Mac against the
# freedesktop specifications and the files this repository already generates.
# Treat the first run as the test.

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

name="pecu"          # the binary, and the icon name the .desktop entry uses
prefix="${HOME}/.local"
uninstall=false

while [ $# -gt 0 ]; do
  case "$1" in
    --prefix) prefix="${2:?--prefix needs a path}"; shift 2 ;;
    --uninstall) uninstall=true; shift ;;
    # The usage block at the top of this file, without the comment markers.
    -h|--help) sed -n '3,7p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

bindir="${prefix}/bin"
icondir="${prefix}/share/icons/hicolor"
appsdir="${prefix}/share/applications"
assets="crates/pecu-app/assets"

# Every size the renderer writes. Kept in step with `LINUX_SIZES` in
# `crates/pecu-ui/examples/render_icon.rs` and with the packaging test.
sizes=(16 24 32 48 64 128 256 512)

if $uninstall; then
  echo "==> removing"
  rm -fv "${bindir}/${name}"
  rm -fv "${appsdir}/${name}.desktop"
  for size in "${sizes[@]}"; do
    rm -fv "${icondir}/${size}x${size}/apps/${name}.png"
  done
  # The caches, so the launcher stops offering something that is gone.
  command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -f -t "$icondir" 2>/dev/null || true
  command -v update-desktop-database >/dev/null && update-desktop-database "$appsdir" 2>/dev/null || true
  echo "removed from ${prefix}"
  exit 0
fi

# ── The things apt has to have supplied ──────────────────────────────────────
#
# Only the ones that are needed to *build*. The runtime libraries — X11,
# Wayland, xkbcommon, GL — are dlopened by winit and glutin rather than linked,
# so a desktop already has them and a bare container does not. The list is in
# the README beside this.
missing=()
command -v cc >/dev/null || missing+=("build-essential")
command -v pkg-config >/dev/null || missing+=("pkg-config")
pkg-config --exists fontconfig 2>/dev/null || missing+=("libfontconfig-dev")
if [ ${#missing[@]} -gt 0 ]; then
  echo "missing build dependencies: ${missing[*]}" >&2
  echo "  sudo apt install ${missing[*]}" >&2
  exit 1
fi

echo "==> building"
# `--locked` so a fresh machine builds the versions this repository was tested
# against rather than whatever resolves today.
cargo build --release --locked -p pecu-app

[ -d "$assets/hicolor" ] || {
  echo "missing $assets/hicolor — run: cargo run -p pecu-ui --example render_icon" >&2
  exit 1
}

echo "==> installing into ${prefix}"

# The binary goes in beside its destination and is then **renamed** over it,
# rather than written straight onto it.
#
# Two reasons, and the second one bites on a re-install:
#
#   * `rename(2)` within a directory is atomic, so there is no instant at which
#     the file on PATH is half-written.
#   * Writing over a binary that is **currently running** fails with `ETXTBSY`
#     — "text file busy" — because the kernel holds the executable mapped. A
#     rename does not touch the old inode at all: a running Pecu keeps the
#     version it started with until it is closed, and the next launch gets the
#     new one. Installing over yourself is the ordinary case here, so it has to
#     be the case that works.
tmp="${bindir}/.${name}.new.$$"
trap 'rm -f "$tmp"' EXIT
install -Dm755 "target/release/${name}" "$tmp"
mv -f "$tmp" "${bindir}/${name}"

for size in "${sizes[@]}"; do
  src="${assets}/hicolor/${size}x${size}/apps/${name}.png"
  [ -f "$src" ] || { echo "missing $src" >&2; exit 1; }
  install -Dm644 "$src" "${icondir}/${size}x${size}/apps/${name}.png"
done

install -Dm644 "${assets}/${name}.desktop" "${appsdir}/${name}.desktop"

# Both are best-effort: a desktop session picks the files up on its own
# eventually, and neither command exists in every environment.
command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -f -t "$icondir" 2>/dev/null || true
command -v update-desktop-database >/dev/null && update-desktop-database "$appsdir" 2>/dev/null || true

echo
echo "installed:"
echo "  ${bindir}/${name}"
echo "  ${icondir}/<size>/apps/${name}.png   (${#sizes[@]} sizes)"
echo "  ${appsdir}/${name}.desktop"
echo

case ":${PATH}:" in
  *":${bindir}:"*) ;;
  *) echo "note: ${bindir} is not on your PATH, so the launcher entry will work"
     echo "      and typing '${name}' will not. Add it to your shell profile." ;;
esac

echo "run it:  ${name}"
echo "         ${name}  --  with a real node, testnet by default"
echo "         (a scripted chain needs a build with --features mock)"
