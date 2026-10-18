#!/bin/sh
# Install psx from a GitHub release (Linux and macOS):
#
#   curl -fsSL https://raw.githubusercontent.com/bombpersons/psx/master/install.sh | sh
#
# Settings (environment variables):
#   PSX_VERSION=v0.2.0      a specific release instead of the latest
#   PSX_INSTALL_DIR=DIR     where the binary goes (default ~/.local/bin)
#   PSX_NO_SIGNATURES=1     skip the PsyQ signatures (only `psx symbols` needs them)
#   PSX_DOWNLOAD_BASE=URL   fetch the archives from here instead (a mirror, or file:// for testing)
#
# Running it again updates psx and the signatures. Nothing else is downloaded:
# BIOS images and discs are yours to supply.
set -eu

repo=bombpersons/psx
sigs_repo=https://github.com/lab313ru/psx_psyq_signatures

say() { printf '%s\n' "$@"; }
fail() { printf 'psx install: %s\n' "$*" >&2; exit 1; }

case "$(uname -s)-$(uname -m)" in
    Linux-x86_64 | Linux-amd64) target=x86_64-unknown-linux-gnu ;;
    Linux-aarch64 | Linux-arm64) target=aarch64-unknown-linux-gnu ;;
    Darwin-arm64) target=aarch64-apple-darwin ;;
    Darwin-x86_64) target=x86_64-apple-darwin ;;
    *) fail "no prebuilt binary for $(uname -s) $(uname -m); build from source with: cargo install --locked --git https://github.com/$repo psx" ;;
esac
command -v curl > /dev/null || fail "curl is required"

if [ -n "${PSX_DOWNLOAD_BASE:-}" ]; then
    base=$PSX_DOWNLOAD_BASE
elif [ -n "${PSX_VERSION:-}" ]; then
    base=https://github.com/$repo/releases/download/$PSX_VERSION
else
    base=https://github.com/$repo/releases/latest/download
fi
asset=psx-$target.tar.gz
dir=${PSX_INSTALL_DIR:-$HOME/.local/bin}

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM
say "Downloading $asset (${PSX_VERSION:-latest release})"
curl -fsSL -o "$tmp/$asset" "$base/$asset" || fail "download failed: $base/$asset"
curl -fsSL -o "$tmp/$asset.sha256" "$base/$asset.sha256" || fail "download failed: $base/$asset.sha256"
expected=$(cut -d ' ' -f 1 < "$tmp/$asset.sha256")
if command -v sha256sum > /dev/null; then
    actual=$(sha256sum "$tmp/$asset" | cut -d ' ' -f 1)
else
    actual=$(shasum -a 256 "$tmp/$asset" | cut -d ' ' -f 1)
fi
[ "$expected" = "$actual" ] || fail "checksum mismatch for $asset"

tar -xzf "$tmp/$asset" -C "$tmp"
mkdir -p "$dir"
cp "$tmp/psx" "$dir/psx.new"
chmod 755 "$dir/psx.new"
mv -f "$dir/psx.new" "$dir/psx"
say "Installed $("$dir/psx" --version) to $dir/psx"

# PsyQ library signatures for `psx symbols`, in the shared data directory psx
# falls back to when a project has no external/psx_psyq_signatures.
sigs=${XDG_DATA_HOME:-$HOME/.local/share}/psx/psyq_signatures
if [ -n "${PSX_NO_SIGNATURES:-}" ]; then
    :
elif ! command -v git > /dev/null; then
    say "git not found: skipped the PsyQ signatures (only psx symbols needs them)"
elif [ -d "$sigs/.git" ]; then
    git -C "$sigs" pull -q --ff-only && say "Updated PsyQ signatures in $sigs" || say "Could not update $sigs"
else
    say "Downloading PsyQ signatures to $sigs"
    mkdir -p "$(dirname "$sigs")"
    git clone -q --depth 1 "$sigs_repo" "$sigs" || say "Could not download the PsyQ signatures (only psx symbols needs them)"
fi

case ":$PATH:" in
    *":$dir:"*) psx=psx ;;
    *) psx=$dir/psx; say "" "$dir is not on your PATH; add it, or use the full path below." ;;
esac
say "" "Next:" \
    "  $psx bios install --from /path/to/bios.bin      # your own PS1 BIOS dump" \
    "  claude mcp add --scope user psx -- $dir/psx mcp  # use it from Claude Code"
