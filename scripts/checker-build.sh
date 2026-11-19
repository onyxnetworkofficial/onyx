#!/usr/bin/env bash
# onyx-check as one ready-made program per system: the proof checker a reader downloads by its own
# name, with no archive to unpack and no Rust to install. Built from this checkout with
# `cargo build --release --locked`, the builder's home folder and the checkout's folder remapped,
# so no path of the build machine is in the file.
#
# Usage: scripts/checker-build.sh <out dir> [target ...]
#   targets: linux-x86_64 macos-arm64 macos-x64 windows-x64
#   default: the targets of this system (Linux: linux-x86_64; macOS: both macOS ones; Windows:
#   windows-x64). Each program is built on its own system: .github/workflows/desktop.yml runs this
#   on a Linux, a macOS and a Windows runner. One exception, for a rehearsal only: on Linux with
#   the MinGW-w64 compiler, `windows-x64` builds for x86_64-pc-windows-gnu (the released file is
#   the Windows runner's, x86_64-pc-windows-msvc with the C runtime linked in, so it needs no
#   redistributable).
# Output: <out dir>/onyx-check-linux-x86_64, onyx-check-macos-arm64, onyx-check-macos-x64,
#   onyx-check-windows-x64.exe. Not code-signed (docs/check-a-proof.md says what a user sees).
# Each program that can run here is smoke-tested: its usage answer (exit 2) and one offline check
# of a recorded answer (exit 0). Refused when a file holds the builder's home folder path.
# Needs: bash, Rust (rust-toolchain.toml) with the target installed, a C compiler for the target.
set -euo pipefail
cd "$(dirname "$0")/.."
usage="usage: $0 <out dir> [linux-x86_64|macos-arm64|macos-x64|windows-x64 ...]"
[ $# -ge 1 ] || { echo "$usage" >&2; exit 2; }
out=$1
shift
mkdir -p "$out"
out=$(cd "$out" && pwd)

have() { command -v "$1" >/dev/null 2>&1; }
home=$HOME
src=$PWD
# What the two folders are called inside the program.
to_home=/build
to_src=/src
case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) host=linux; [ $# -gt 0 ] || set -- linux-x86_64 ;;
  Darwin-arm64 | Darwin-x86_64) host=macos; [ $# -gt 0 ] || set -- macos-arm64 macos-x64 ;;
  MINGW*-x86_64 | MSYS*-x86_64 | CYGWIN*-x86_64)
    host=windows
    [ $# -gt 0 ] || set -- windows-x64
    # rustc and cargo see Windows paths (C:\Users\...), not the shell's (/c/Users/...).
    home=$(cygpath -w "$HOME")
    src=$(cygpath -w "$PWD")
    # No leading slash here: this shell rewrites what looks like a path of its own (/build) into
    # a Windows one, inside the variables it hands to cargo too.
    to_home=onyx-build
    to_src=onyx-src ;;
  *) echo "unsupported build machine: $(uname -s) $(uname -m)" >&2; exit 2 ;;
esac

# target: the Rust target, the file's name, and whether this system builds it.
plan() {
  extra=
  case $1 in
    linux-x86_64) triple=x86_64-unknown-linux-gnu; name=onyx-check-linux-x86_64; exe=onyx-check; on=linux ;;
    macos-arm64) triple=aarch64-apple-darwin; name=onyx-check-macos-arm64; exe=onyx-check; on=macos ;;
    macos-x64) triple=x86_64-apple-darwin; name=onyx-check-macos-x64; exe=onyx-check; on=macos ;;
    windows-x64)
      name=onyx-check-windows-x64.exe; exe=onyx-check.exe; on=windows
      if [ "$host" = linux ]; then
        have x86_64-w64-mingw32-gcc || { echo "windows-x64 on Linux needs the MinGW-w64 compiler (a rehearsal build); the released file is built by .github/workflows/desktop.yml" >&2; exit 2; }
        triple=x86_64-pc-windows-gnu; on=linux
      else
        # The C runtime inside the program: it starts on a Windows with no Visual C++ files installed.
        triple=x86_64-pc-windows-msvc; extra=" -C target-feature=+crt-static"
      fi ;;
    *) echo "unknown target $1" >&2; echo "$usage" >&2; exit 2 ;;
  esac
  [ "$on" = "$host" ] || { echo "$1 is built on its own system ($on), by .github/workflows/desktop.yml; this machine is $host" >&2; exit 2; }
}
for t in "$@"; do plan "$t"; done

case "$home$src" in *" "*) echo "the home folder or the checkout has a space in its path: the path remap cannot be passed" >&2; exit 1 ;; esac
target=${CARGO_TARGET_DIR:-$PWD/target}
# The same macOS floor as the desktop app (macOS 11), for the Rust code and the C code alike.
export MACOSX_DEPLOYMENT_TARGET=${MACOSX_DEPLOYMENT_TARGET:-11.0}
export CARGO_INCREMENTAL=0

SHARD=0xacbcbfa203b99d9cba691cc1958b3c6563f04f8eab94db5a0402597b1ca34244
smoke() { # program, runner words: the usage answer, then one offline check of a recorded answer
  local bin=$1 code=0 said
  shift
  said=$("$@" "$bin" 2>/dev/null) || code=$?
  [ "$code" = 2 ] || { echo "smoke: $bin without arguments exits $code, not 2" >&2; exit 1; }
  case $said in *'"code":"usage"'*) ;; *) echo "smoke: $bin gives no usage answer: $said" >&2; exit 1 ;; esac
  code=0
  said=$("$@" "$bin" --answer crates/onyx-check/tests/fixtures/proof.json \
    --asked "{\"kind\":\"proof\",\"round\":1,\"shard\":\"$SHARD\",\"index\":0}" --unchecked 2>/dev/null) || code=$?
  [ "$code" = 0 ] || { echo "smoke: $bin refuses the recorded answer (exit $code): $said" >&2; exit 1; }
  case $said in *'"ok":true'*'"chainChecked":false'* | *'"chainChecked":false'*'"ok":true'*) ;;
    *) echo "smoke: $bin gives an unexpected report: $said" >&2; exit 1 ;; esac
  echo "  smoke-tested: $(basename "$bin")"
}

for t in "$@"; do
  plan "$t"
  # The C code's file names too (gcc and clang; Microsoft's compiler has no such switch, and the
  # check below refuses a file that holds a path either way).
  cmap=
  [ "$triple" = x86_64-pc-windows-msvc ] || cmap="-ffile-prefix-map=$home=$to_home -ffile-prefix-map=$src=$to_src"
  cvar=CFLAGS_${triple//-/_}
  env "$cvar=${!cvar:-} $cmap" \
    RUSTFLAGS="--remap-path-prefix=$home=$to_home --remap-path-prefix=$src=$to_src$extra" \
    cargo build -q --locked --release -p onyx-check --target "$triple"
  cp "$target/$triple/release/$exe" "$out/$name"
  chmod +x "$out/$name"
  # No path of the build machine in the file (either slash on Windows).
  for p in "$home" "${home//\\//}" "$src" "${src//\\//}"; do
    if grep -qaF -- "$p" "$out/$name"; then
      rm -f "$out/$name"
      echo "$name holds a folder path of the build machine (the home folder or the checkout): refused" >&2
      exit 1
    fi
  done
  case $t in
    linux-x86_64 | windows-x64)
      if [ "$triple" = x86_64-pc-windows-gnu ]; then
        # A rehearsal on Linux: run it only where Wine is installed.
        if have wine; then WINEDEBUG=-all smoke "$out/$name" wine; else echo "  not run here (no Wine): $name"; fi
      else
        smoke "$out/$name" env
      fi ;;
    macos-arm64)
      if [ "$(uname -m)" = arm64 ]; then smoke "$out/$name" env; else echo "  not run here (an Intel Mac): $name"; fi ;;
    macos-x64)
      # On Apple silicon the Intel program runs under Rosetta, where that is installed.
      if [ "$(uname -m)" = x86_64 ]; then smoke "$out/$name" env
      elif arch -x86_64 /usr/bin/true 2>/dev/null; then smoke "$out/$name" arch -x86_64
      else echo "  not run here (no Rosetta): $name"; fi ;;
  esac
done
echo "onyx-check in $out:"
(cd "$out" && ls -l onyx-check-*)
