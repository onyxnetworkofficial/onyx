#!/usr/bin/env bash
# onyx-contrib as single-file programs that need no Node install (CL10, ADR-0061): Node's single
# executable application (SEA). The program is Node's official binary for the target, downloaded
# from nodejs.org and checked against its SHASUMS256.txt, with onyx-contrib written into it as one
# CommonJS file (scripts/sea-bundle.mjs) by postject (pinned in scripts/desktop-tools).
#
# Usage: scripts/desktop-build.sh <out dir> [target ...]
#   targets: linux-x64 windows-x64 macos-arm64 macos-x64 macos-universal
#   default: every target this machine can build. macOS programs must be signed (ad hoc here,
#   `codesign --sign -`) or Apple silicon refuses to run them, so they build only on macOS; Linux
#   and Windows programs build anywhere (the Windows one's old signature is cut off first,
#   scripts/pe-unsign.mjs). The program for this machine is smoke-tested (scripts/desktop-smoke.mjs).
# Output: onyx-contrib-<target>[.exe] and SHA256SUMS in <out dir>. The desktop app's workflow
# (.github/workflows/desktop.yml) runs this on each system and uses the result as its sidecar.
# Needs: bash, curl, tar, Node 20+ and npm (to run the bundler and postject), sha256sum or shasum,
# unzip, python3 or PowerShell (Windows' zip). Caches downloads in ONYX_NODE_CACHE (default ~/.cache/onyx-node-dist).
set -euo pipefail
cd "$(dirname "$0")/.."
NODE_VERSION=24.22.2
FUSE=NODE_SEA_FUSE_fce680ab2cc467b6e072b8b5df1996b2
[ $# -ge 1 ] || { echo "usage: $0 <out dir> [linux-x64|windows-x64|macos-arm64|macos-x64|macos-universal ...]" >&2; exit 2; }
out=$1
shift
mkdir -p "$out"
out=$(cd "$out" && pwd)
cache=${ONYX_NODE_CACHE:-$HOME/.cache/onyx-node-dist}
mkdir -p "$cache"
work=$(mktemp -d "$cache/build.XXXXXX")
trap 'rm -rf "$work"' EXIT

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) host=linux-x64 ;;
  Darwin-arm64) host=macos-arm64 ;;
  Darwin-x86_64) host=macos-x64 ;;
  MINGW*-x86_64 | MSYS*-x86_64 | CYGWIN*-x86_64) host=windows-x64 ;;
  *) echo "unsupported build machine: $(uname -s) $(uname -m)" >&2; exit 2 ;;
esac
have() { command -v "$1" >/dev/null 2>&1; }
sha256() { if have sha256sum; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi; }

if [ $# -eq 0 ]; then
  set -- linux-x64 windows-x64
  [ "$host" = macos-arm64 ] || [ "$host" = macos-x64 ] && set -- "$@" macos-arm64 macos-x64 macos-universal
fi
for t in "$@"; do
  case $t in
    linux-x64 | windows-x64) ;;
    macos-arm64 | macos-x64 | macos-universal)
      have codesign && have lipo || { echo "$t: needs macOS (codesign, lipo); skipped here, built by .github/workflows/desktop.yml" >&2; exit 2; } ;;
    *) echo "unknown target $t" >&2; exit 2 ;;
  esac
done

# Node's official archive for a platform, checked against SHASUMS256.txt; prints the binary's path.
node_dist() {
  local plat=$1 file member
  case $plat in
    linux-x64) file=node-v$NODE_VERSION-linux-x64.tar.xz; member=node-v$NODE_VERSION-linux-x64/bin/node ;;
    darwin-arm64) file=node-v$NODE_VERSION-darwin-arm64.tar.gz; member=node-v$NODE_VERSION-darwin-arm64/bin/node ;;
    darwin-x64) file=node-v$NODE_VERSION-darwin-x64.tar.gz; member=node-v$NODE_VERSION-darwin-x64/bin/node ;;
    win-x64) file=node-v$NODE_VERSION-win-x64.zip; member=node-v$NODE_VERSION-win-x64/node.exe ;;
  esac
  local dir=$cache/v$NODE_VERSION
  mkdir -p "$dir"
  [ -s "$dir/SHASUMS256.txt" ] || curl -fsSL --proto '=https' -o "$dir/SHASUMS256.txt" "https://nodejs.org/dist/v$NODE_VERSION/SHASUMS256.txt"
  local want
  want=$(awk -v f="$file" '$2 == f {print $1}' "$dir/SHASUMS256.txt")
  [ -n "$want" ] || { echo "no checksum for $file" >&2; exit 1; }
  if [ ! -s "$dir/$file" ] || [ "$(sha256 "$dir/$file")" != "$want" ]; then
    curl -fsSL --proto '=https' -o "$dir/$file.part" "https://nodejs.org/dist/v$NODE_VERSION/$file"
    mv "$dir/$file.part" "$dir/$file"
  fi
  [ "$(sha256 "$dir/$file")" = "$want" ] || { echo "$file: SHA-256 does not match nodejs.org's SHASUMS256.txt" >&2; exit 1; }
  local bin=$dir/$plat/$(basename "$member")
  if [ ! -s "$bin" ]; then
    mkdir -p "$dir/$plat"
    case $file in
      *.zip)
        if have unzip; then unzip -p "$dir/$file" "$member" >"$bin.part"
        elif have python3; then python3 -I -c 'import sys, zipfile; sys.stdout.buffer.write(zipfile.ZipFile(sys.argv[1]).read(sys.argv[2]))' "$dir/$file" "$member" >"$bin.part"
        else # Windows without unzip or python3: PowerShell's own zip reader
          rm -rf "$work/zip" && powershell.exe -NoProfile -NonInteractive -Command "Expand-Archive -LiteralPath '$(cygpath -w "$dir/$file")' -DestinationPath '$(cygpath -w "$work/zip")'"
          cp "$work/zip/$member" "$bin.part"
        fi ;;
      *) tar -xOf "$dir/$file" "$member" >"$bin.part" ;;
    esac
    chmod +x "$bin.part"
    mv "$bin.part" "$bin"
  fi
  echo "$bin"
}

case $host in
  linux-x64) host_plat=linux-x64 ;;
  macos-arm64) host_plat=darwin-arm64 ;;
  macos-x64) host_plat=darwin-x64 ;;
  windows-x64) host_plat=win-x64 ;;
esac
hostnode=$(node_dist "$host_plat")

# postject, pinned with its lock file, installed outside the repository.
tools=$cache/tools
mkdir -p "$tools"
cp scripts/desktop-tools/package.json scripts/desktop-tools/package-lock.json "$tools/"
(cd "$tools" && npm ci --no-audit --no-fund --loglevel=error >/dev/null)
postject=$tools/node_modules/postject/dist/cli.js

# The bundle and the SEA blob (made by the same Node version as every target binary; no code cache
# or snapshot, so one blob serves every platform).
node scripts/sea-bundle.mjs "$work/onyx-contrib.cjs" >/dev/null
cat >"$work/sea-config.json" <<EOF
{ "main": "onyx-contrib.cjs", "output": "sea.blob", "disableExperimentalSEAWarning": true, "useSnapshot": false, "useCodeCache": false }
EOF
(cd "$work" && "$hostnode" --experimental-sea-config sea-config.json >/dev/null)

inject() { # target plat file
  local t=$1 plat=$2 f=$3
  cp "$(node_dist "$plat")" "$f"
  chmod u+w "$f"
  case $t in
    windows-x64) node scripts/pe-unsign.mjs "$f" >/dev/null ;;
    macos-*) codesign --remove-signature "$f" ;;
  esac
  local extra=()
  case $t in macos-*) extra=(--macho-segment-name NODE_SEA) ;; esac
  node "$postject" "$f" NODE_SEA_BLOB "$work/sea.blob" --sentinel-fuse "$FUSE" "${extra[@]}" >/dev/null 2> >(grep -v "Can't find string offset for section name" >&2)
  case $t in macos-*) codesign --sign - --force "$f" ;; esac
  chmod 755 "$f"
}

built=()
for t in "$@"; do
  case $t in
    linux-x64) inject "$t" linux-x64 "$out/onyx-contrib-linux-x64"; built+=(onyx-contrib-linux-x64) ;;
    windows-x64) inject "$t" win-x64 "$out/onyx-contrib-windows-x64.exe"; built+=(onyx-contrib-windows-x64.exe) ;;
    macos-arm64) inject "$t" darwin-arm64 "$out/onyx-contrib-macos-arm64"; built+=(onyx-contrib-macos-arm64) ;;
    macos-x64) inject "$t" darwin-x64 "$out/onyx-contrib-macos-x64"; built+=(onyx-contrib-macos-x64) ;;
    macos-universal)
      [ -s "$out/onyx-contrib-macos-arm64" ] || inject macos-arm64 darwin-arm64 "$out/onyx-contrib-macos-arm64"
      [ -s "$out/onyx-contrib-macos-x64" ] || inject macos-x64 darwin-x64 "$out/onyx-contrib-macos-x64"
      lipo -create "$out/onyx-contrib-macos-arm64" "$out/onyx-contrib-macos-x64" -output "$out/onyx-contrib-macos-universal"
      codesign --sign - --force "$out/onyx-contrib-macos-universal"
      built+=(onyx-contrib-macos-universal) ;;
  esac
done

# Nothing private ships: no home folder path in any program.
for f in "${built[@]}"; do
  ! grep -qaF "$HOME" "$out/$f" || { echo "$f holds the builder's home folder path: refused" >&2; exit 1; }
done

# Smoke-test the program for this machine, when it was built.
hostbin=onyx-contrib-$host
[ "$host" = windows-x64 ] && hostbin=onyx-contrib-windows-x64.exe
for f in "${built[@]}"; do
  [ "$f" = "$hostbin" ] && node scripts/desktop-smoke.mjs "$out/$f"
done

(cd "$out" && for f in "${built[@]}"; do printf '%s  %s\n' "$(sha256 "$f")" "$f"; done >SHA256SUMS)
echo "desktop-build: Node $NODE_VERSION, in $out:"
(cd "$out" && for f in "${built[@]}"; do echo "  $f $(wc -c <"$f" | tr -d ' ') bytes"; done)
