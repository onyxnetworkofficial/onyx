# Remote check host (ROADMAP E13)

## Scope

`scripts/remote-check.sh` runs `scripts/check-all.sh` for one worktree on a second machine, so the
heavy gate (cargo, wasm, forge, the three web builds and their browser e2e) leaves the dev box,
which runs at load 40-60 with several agents. The host is the build machine, as its separate `onyx-build` user (ssh alias `dell-onyx`),
approved for Onyx builds on 2026-10-18. It is a shared production machine (it runs a vendor's
indexing), so every rule below protects that work first. Threats: THREATS.md REMOTE-1..10.

## Use

```sh
scripts/remote-check.sh --log /path/to/run.log <worktree> ONYX_WEB_PUBLIC=1
echo $?   # check-all's exit status on the host
```

| Exit | Meaning |
|---|---|
| 0 | `ALL CHECKS PASSED` on the host (the status is check-all's, passed back unchanged; both scripts are parsed whole before they run, so a checkout or edit of them during a run cannot change it, REMOTE-9) |
| 1 (or check-all's own) | a check failed: read the log |
| 2 | refused here (arguments, a secret, a never-synced path, recipes/out, a submodule not checked out) |
| 3 | refused by the host (load, free disk, Onyx's disk budget, no build slot free within 2 hours, no Landlock) |
| 4 | stopped by the host's disk watchdog during the run |
| 255 | ssh or the copy to the host failed (an unreachable host included) |

Other modes: `--dry-run <worktree>` (every local refusal and the file list, no host),
`--clean <branch>` (deletes `~/onyx-build/src/<branch>` and `~/onyx-build/target/<branch>` on the
host, nothing else; the branch name with `/` mapped to `-`), `--setup` (copies the tools and the
fixture blocks, idempotent), `--self-test` (run by check-all; no host). The host is
`ONYX_REMOTE_HOST` (default `dell-onyx`).

Only `ONYX_NAME=VALUE` variables pass through (letters, digits and `_.:,/+-` in the value).
`ONYX_DOCKER` is refused and always 0 on the host: the Docker smoke (`ONYX_DOCKER=1
scripts/check-all.sh`) runs only on the dev box. `ONYX_RESERVE_DIR` and `ONYX_LIVE_DIR` are set by
the host to its fixture copy.

## What happens

1. Here: the worktree's own `scripts/check-secrets.sh`; the file list (tracked files minus deleted
   ones, submodule files, untracked files that are not ignored); the same name and content rules
   over that list (`scripts/check-secrets.sh --paths-from`, so an untracked key or env file is
   caught too), plus, for the list only, credential files by name (`secrets/`, `.ssh/`, `id_ed25519`,
   `.npmrc`, `.netrc`, wallet and key-store JSON, `.p12` and other key stores, which are often binary)
   and token forms by content (GitHub, npm, AWS, Slack, `sk-` keys, quoted or env-style tokens and
   passwords, URLs holding a password); a refusal for any path under `target/`, `node_modules/`,
   `apps/web/build/`, `.onyx-testnet/` or `.git/`, and for any path under a symlinked folder (a
   submodule folder replaced by a link to another checkout, which rsync would follow); check-all's
   first gate (recipes/out against the index), which the host cannot see. Ignored files are never
   listed. Git runs with `GIT_OPTIONAL_LOCKS=0`: the worktree is only read.
2. rsync of exactly that list to `~/onyx-build/src/<branch>/` (symlinks as links, never followed;
   env, key and build names excluded again as a second layer; the self-test runs the same rsync into
   a local folder and compares it with the list). Files gone since the last sync are deleted there; the
   host rebuilds the index from the list, submodules as nested repositories, so `git ls-files` and
   `git diff` in the scripts see what they see here.
3. On the host (`scripts/remote-check-runner.sh`): wait for one of 2 build slots (at most two
   runs at once, REMOTE-10; up to 2 hours, then exit 3), then refuse at a 1-minute load above 60, under 60 GB
   free on `/`, or with `~/onyx-build` plus `~/onyx-tools` over 80 GB. Then one systemd user scope:
   `systemd-run --user --scope -p MemoryMax=48G -p MemorySwapMax=0 -p CPUQuota=4800% nice -n 19
   ionice -c 3 env -i ... remote-check-runner.sh sandbox <rules> -- ... job` (every child, rustc,
   node and Chrome included, inherits the scope, nice and I/O class; no swap, so 48 GB is the whole
   memory). `sandbox` is a Landlock file-system sandbox (no root needed; see REMOTE-4 below): the
   job can write only `~/onyx-build/src/<branch>`, `target/<branch>`, `tmp/<branch>`, the private
   `HOME` and `~/onyx-tools/cargo-home`, can read only the system folders and Onyx's two folders, and
   cannot read or write the rest of the home, `/mnt` or `/tmp`; the runner refuses to start it on a
   kernel without Landlock (exit 3). Inside: a private `HOME` and `TMPDIR` under `~/onyx-build`,
   `CARGO_TARGET_DIR=~/onyx-build/target/<branch>`, `CARGO_INCREMENTAL=0` (CI style) and
   `CARGO_PROFILE_DEV_DEBUG=line-tables-only` (a full-debuginfo target is about 35 GB; backtraces
   keep file:line), `RAYON_NUM_THREADS=16` (a test-environment setting only: it makes host.rs's OOM
   probe pass on the 96-thread host as on the dev box and hides THREATS REMOTE-5, a real bug that
   stays open until TODO E1), `cargo fetch --locked`, then for a web gate
   the demo export with `ONYX_LIVE=<host copy>/node:<first>:<last>` (gitignored, so never synced),
   then `scripts/check-all.sh`, then the old duplicate build files are swept (the logic of
   `scripts/sweep-target.py` over every `deps` folder).
4. A watchdog stops the job's own scope below 45 GB free or over the 80 GB budget (exit 4). If the
   ssh session drops (no tty, so no SIGHUP: the runner sees its output pipe close at the next log
   line) or the runner gets a signal, it stops the same scope; it also stops the scope when the job
   ends, so a test server or browser that outlived the job dies with it. Nothing else on the host is
   ever signalled.
5. The log streams into the local file; the host keeps its 20 newest logs in `~/onyx-build/logs`.
   The runner's own lines print host paths as `~/...` (the host's home folder names its account);
   check-all's and cargo's output can still hold absolute host paths, so the log stays local and is
   never committed.

Measured 2026-10-19: a full run with `ONYX_WEB_PUBLIC=1` from an empty target took 5,478 s on
the host (export about 7 min, workspace tests about 50 min, three web builds with e2e about 25 min);
the branch then held about 14 GB (Onyx 24 GB in all with tools and fixtures).

## Host layout and tools

Everything Onyx writes on the host is in two folders. Nothing is installed system-wide and no
config outside them changes; tools are this machine's own copies (no downloads).

| Path | Holds | Size (2026-10-18) |
|---|---|---|
| `~/onyx-tools/rust` | Rust 1.98.0 with the wasm32 target, clippy, rustfmt | 697 MB |
| `~/onyx-tools/node`, `pnpm`, `bin/pnpm` | Node 22.23.2, pnpm 10.34.5 (the dev box's copy) | 230 MB |
| `~/onyx-tools/foundry/bin`, `svm` | forge, anvil, cast, chisel 1.8.5; solc 0.8.30 | about 300 MB |
| `~/onyx-tools/bin` | wasm-bindgen 0.2.129, cargo-deny 0.20.2 (the dev box's builds run there) | about 60 MB |
| `~/onyx-tools/cargo-home` | crates fetched by `cargo fetch --locked` | filled on first run |
| `~/onyx-build/fixtures-live` | Bitcoin block hex and block JSON only (checked by `--setup`) | 8.9 GB |
| `~/onyx-build/src/<branch>`, `target/<branch>` | synced sources, cargo target | per branch |
| `~/onyx-build/home`, `tmp`, `logs`, `bin` | private HOME (pnpm store), TMPDIR, logs, the runner | small |

The host's own clang 18 builds blst and secp256k1 for wasm32, and its Google Chrome
(`/opt/google/chrome`) runs the browser e2e (`channel: 'chrome'`), as on the dev box: nothing of
Playwright's browser downloads is needed. crates.io, the npm registry and the RustSec database are
reachable from the host, so `cargo fetch`, `pnpm install --frozen-lockfile` (with
`prefer-offline`: the store first) and `cargo deny`'s advisories work as here.

## Assumptions

- Since 2026-10-19 the alias `dell-onyx` logs in as `onyx-build`, a separate Unix user made for
  Onyx (no password, no sudo, its own `restrict`ed key `~/.ssh/id_ed25519_onyx_build`; the project owner's
  home is mode 750, so it cannot read it). Onyx code runs as that user, inside the Landlock
  sandbox (REMOTE-4). Undo: `sudo userdel -r onyx-build` on the build machine and remove the alias.
- The host's kernel has Landlock (6.8, ABI 4: checked 2026-10-19). systemd's own sandbox settings
  (`ProtectHome=`, `BindPaths=`, `PrivateTmp=`) do not work there without root: the host's AppArmor
  blocks unprivileged user namespaces, so a user unit ignores them silently (`ls ~` still lists the
  home) or fails with status 226 (`TemporaryFileSystem=`).

## What the sandbox does not cover (REMOTE-4)

Landlock on kernel 6.8 restricts files only. Branch code still runs as the host's user and can:
signal that user's processes (the Cardano indexers, its user services), connect to Unix sockets
(the user's systemd and D-Bus buses, so `systemctl --user` could stop production services;
PostgreSQL's socket with peer authentication), and open any TCP connection (localhost included).
`env -i` removes the variables that point at those buses, which stops accidents, not a deliberate
attack. Closing these needs a separate Unix user (or a container) on the host: Needs the project owner.
- Branch folders are cleaned after merge (`--clean <branch>`): two full targets fit in the budget,
  three may not.

## Open questions

- A separate Unix user on the host for Onyx builds (REMOTE-4, Needs the project owner).
