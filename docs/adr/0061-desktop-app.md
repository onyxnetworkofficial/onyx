# ADR-0061: The desktop app: a local app server, single-file programs and a Tauri window

## Status
Accepted, 2026-10-30 (docs/contributor-loop.md CL10; test network only). Built:
`onyx-contrib ui` (apps/contrib/src/ui.mjs, the page in apps/contrib/ui), the single-file programs
(scripts/sea-bundle.mjs, scripts/desktop-build.sh), the Tauri shell (apps/desktop), the installer
workflow (.github/workflows/desktop.yml) and the website's Download page (apps/web, `#/download`).
User guide: docs/desktop-app.md. Threats: THREATS.md section CL10. The first installers are in
v0.3.1-testnet (2026-11-01, built by the workflow on the tag; v0.3.0-testnet's macOS and Windows
builds failed and it has the command-line files only).

## Context
Contributing data with onyx-contrib (CL2, CL4) needs a terminal, Node.js 20 and an MCP client's
settings file edited by hand. The owner's aim is the Bitcoin Core experience: one big button on the
website, an installer for your system, double-click, a window. Nothing about keys may get weaker on
the way: a key never reaches Onyx, the page, a log or any other host (THREATS CL2), and the app
must not become a door other programs or web pages on the same computer can use.

This machine has no webkit2gtk (and no system packages may be installed), so the window cannot
be built here; the three systems' build machines are GitHub's runners, with the repository's own
token and no other secret. Code signing needs paid certificates (Apple Developer ID, a Windows
code-signing certificate) and is the owner's decision.

## Decision
1. **One program does the work: onyx-contrib.** `onyx-contrib ui` serves a small app (plain
   HTML, CSS and JavaScript, no outside asset, no framework) from a local web server: add a
   provider, store its key (typed once, sent once to the local server, put in the keychain, never
   shown again), list endpoints, read a value, check with Onyx (records nothing), send after an
   explicit tick and press, past contributions with their status, a plain-words health check
   (for example "Install libsecret-tools", or the encrypted-file vault, chosen in the app with a
   passphrase kept in memory only), and "Connect to Claude Desktop". The window, the browser and
   Claude Desktop all reach the same code the MCP server runs (`runTool`), so the key filter, the
   origin binding and the confirm rule are the CL2 ones, not copies.
2. **Who may call the server.** It listens on 127.0.0.1 only, on a random port. A per-launch
   secret (32 random bytes) is required on every API call (`x-onyx-token`, constant-time
   compare); the page receives it in the URL fragment (`#t=…`), which browsers never send to a
   server or in a Referer, and removes it from the address bar. Host must be `127.0.0.1:<port>` or
   `localhost:<port>` on every request (DNS rebinding); Origin, when sent, must be the server's
   own, and every write must send it; `Sec-Fetch-Site` must be same-origin or none; writes are
   JSON only; no CORS answer is ever given; responses carry a strict CSP, `X-Frame-Options: DENY`
   and `no-store`. A browser started from a terminal opens a private file (0600, deleted after a minute)
   that forwards to the tokened URL, so its command line, which other users can read, holds no
   secret. A request whose input holds a piece of a key the session knows is refused and the
   session stops using keys, so the key filter can never be used to test guesses (review CL10-R1). Every answer
   and log line passes the CL2 key filter for every key read or typed and the vault passphrase.
3. **Claude Desktop.** "Connect" merges `mcpServers["onyx-contrib"]` into Claude Desktop's
   settings file (macOS `~/Library/Application Support/Claude/`, Windows `%APPDATA%\Claude\`,
   Linux `~/.config/Claude/`), copying the old file to a dated backup first; other servers and
   settings are kept; a file that is not valid JSON is left alone. The entry runs this program with
   `mcp`; from an AppImage (whose folder changes every launch) the program is first copied to the
   settings folder. No key and no passphrase is ever written there.
4. **No Node install: Node's single executable application (SEA).** Node 24.22.2's official
   binary for each system, checked against nodejs.org's SHASUMS256.txt, with onyx-contrib written
   in by postject (pinned with its lock file). SEA runs one CommonJS script, so
   scripts/sea-bundle.mjs turns our ES modules into one file; it reads only the module syntax our
   files use and refuses the rest (default exports, re-exports, packages, cycles, top-level await),
   so a new construct fails the build, never the program. The app's page and the agent's OpenAPI
   and catalog files are embedded and served to their own `import.meta.url` paths. macOS programs
   are signed ad hoc (`codesign --sign -`, required on Apple silicon) and so build on macOS only;
   the Windows program's old Authenticode signature is cut off before writing
   (scripts/pe-unsign.mjs, what `signtool remove /s` does). Targets: linux-x64, macos-arm64,
   macos-x64 (and a universal one for the app), windows-x64.
5. **The window: Tauri 2** (apps/desktop). It starts the SEA as a sidecar with `ui --desktop`,
   reads the one line it prints (`ONYX_UI_URL=…`, its shape checked), and opens its window on it;
   the window may show only its bundled start page and that one local origin. Quitting closes the
   sidecar's input (it exits when its input closes) and stops it. Its own Cargo workspace,
   excluded from the repository's, so `cargo` and check-all on machines without the system web
   view are unaffected.
6. **Installers by GitHub Actions** (`.github/workflows/desktop.yml`), on a pushed `v*-testnet`
   tag: Windows `.msi` and NSIS `.exe`, macOS one universal `.dmg`, Linux `.AppImage` and `.deb`,
   plus the single-file programs and `SHA256SUMS` (merged with the release's own), under stable
   names, attached with the repository's `GITHUB_TOKEN`. Every action pinned to a full commit SHA;
   the token has no permission by default, `contents: read` for the builds and `contents: write`
   only in the release job (checked by scripts/check-workflows.py in check-all). Unsigned for now;
   docs/desktop-app.md tells users the one extra click Windows SmartScreen and macOS Gatekeeper ask
   for, in plain words.
7. **The Download page** (`#/download`): one big button for the visitor's system, the other
   systems below it, SHA256SUMS, the unsigned-installer steps and the command-line downloads under
   "For developers". One constant names the release (`RELEASE` in apps/web/src/lib/release.ts);
   the installers are linked only from the first release the workflow builds (`DESKTOP_SINCE`),
   so the page never links a file that does not exist.

## Alternatives
- **Electron** (https://www.electronjs.org/): one Chromium on every system and Node built in, so
  no sidecar. Rejected: an installer of about 80-120 MB that ships its own browser engine to keep
  patched, and a Node runtime inside the window's process unless carefully isolated. Tauri
  (https://v2.tauri.app/) uses the system web view (WebView2, WKWebView, WebKitGTK), gives a few
  MB of shell, makes the windows' IPC opt-in per origin (none here), and bundles `.msi`, NSIS,
  `.dmg`, `.AppImage` and `.deb` from one config.
- **Tauri with the logic in Rust** (porting onyx-contrib): one language in the app, but a second
  implementation of the vault, the key filter and the intake rules to keep equal to CL2's.
  Rejected for now: one implementation, tested once, used by the CLI, the MCP server and the app.
- **A bundler** (esbuild, Rollup) instead of scripts/sea-bundle.mjs: general, but a build
  dependency and a bigger surface for a dozen files of our own. The small bundler fails closed.
- **Node's newer `--build-sea`** (Node 25.5+) or ESM entry points for SEA: not in an LTS line yet.
- **No local server** (the window talks to Rust commands only): no page for a browser user, and
  Claude Desktop's MCP server still needs the program. The local server has the gates of item 2.
- **Token in a cookie**: cookies for 127.0.0.1 are shared across ports, so another local server
  the browser visits would receive it. The fragment plus a header keeps it in this origin.
- **Signing now**: needs paid certificates and an Apple account; the owner decides (ROADMAP S20).

## Consequences
- Contributors on Windows, macOS and Linux get an installer and a window; the terminal and Node
  stay optional. The same program is Claude Desktop's MCP server.
- Until signing, Windows and macOS ask once before opening the app; the guide and the Download
  page say how. Users can check files against SHA256SUMS.
- The single-file programs are large (about 90-130 MB: Node itself). Accepted for v0.
- The installers are built only by CI. What was verified on this machine: the app server and its
  gates (tests), the bundle (tests and a smoke run with no Node on PATH), the Linux program (smoke
  test), the Windows program under Wine (version, doctor, MCP, the app served with its gates), and
  the Tauri shell compiled and linked for Windows (x86_64-pc-windows-gnu) with its unit tests run
  under Wine. Not verified here: a window on any system, the macOS programs, the installers.
- A later release only changes `RELEASE` on the website.
