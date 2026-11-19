# Check a proof on macOS or Windows

Test network only (Base Sepolia): no real money.

Every row the API returns comes with a proof. The program `onyx-check` checks it on your own
computer: the publisher's signature, and the record on Base Sepolia read through a chain node you
choose. It prints a report and exits 0 only when everything holds.

From the test-network release `v0.5.1-testnet` on, `onyx-check` is one ready-made file per system
on the [release page](https://github.com/onyxnetworkofficial/onyx/releases), next to `SHA256SUMS`.
An earlier release has it for Linux only, inside `onyx-publish-linux-x86_64.tar.gz`; on another
system build it from that release's source with `cargo build --release -p onyx-check`.

The files are not code-signed yet (signing comes later), so macOS and Windows ask once before the
first start. The steps below include that.

## macOS

1. Download `onyx-check-macos-arm64` (Apple silicon) or `onyx-check-macos-x64` (Intel), and
   `SHA256SUMS`, from the release page. The lines below use the Apple silicon name.
2. Open Terminal in the download folder and run `shasum -a 256 onyx-check-macos-arm64`. The value
   must equal the file's line in `SHA256SUMS`.
3. Allow it to run: `chmod +x onyx-check-macos-arm64`, then
   `xattr -d com.apple.quarantine onyx-check-macos-arm64` (it takes off the mark a browser puts on
   a download; "No such xattr" means there was none).
4. If macOS still refuses the first start: System Settings, Privacy & Security, Open Anyway.
5. Save an answer:
   `curl -s "https://onyx.cryptagraph.com/v1/bulk/onchain-archive/btc.entity-adjusted-mvrv/rows?order=desc&limit=5" > rows.json`
6. Check it:
   `./onyx-check-macos-arm64 --bulk rows.json --publisher onchain-archive --dataset btc.entity-adjusted-mvrv --order desc --rpc https://sepolia.base.org --chain-id 84532 --book 0x04bb31d80c8ce9df02c8a03486b5812ec6e5ccc5`
   The report must say `"ok":true`, and `echo $?` must print `0`.

## Windows

1. Download `onyx-check-windows-x64.exe` and `SHA256SUMS` from the release page.
2. Open Command Prompt in the download folder (not PowerShell: its `curl` and `>` work
   differently) and run `certutil -hashfile onyx-check-windows-x64.exe SHA256`. The value must
   equal the file's line in `SHA256SUMS`.
3. Allow it to run: if "Windows protected your PC" appears at the first start, choose More info,
   then Run anyway. Or before the first start: right-click the file, Properties, tick Unblock, OK.
4. Save an answer:
   `curl -s -o rows.json "https://onyx.cryptagraph.com/v1/bulk/onchain-archive/btc.entity-adjusted-mvrv/rows?order=desc&limit=5"`
5. Check it:
   `onyx-check-windows-x64.exe --bulk rows.json --publisher onchain-archive --dataset btc.entity-adjusted-mvrv --order desc --rpc https://sepolia.base.org --chain-id 84532 --book 0x04bb31d80c8ce9df02c8a03486b5812ec6e5ccc5`
6. The report must say `"ok":true`, and `echo %ERRORLEVEL%` must print `0`.

## Linux

Download `onyx-check-linux-x86_64` (x86-64, Ubuntu 22.04, Debian 12 or newer) and `SHA256SUMS`;
`sha256sum -c --ignore-missing SHA256SUMS` must say `OK`; `chmod +x onyx-check-linux-x86_64`; then
steps 5 and 6 of macOS with `./onyx-check-linux-x86_64`.

## More

- Other reads (a key range, a later page, a moved dataset) and what each line of the report means:
  the API page of the website, "Check an answer yourself", and `docs/agents.md`.
- The program is built by `scripts/checker-build.sh` on a Linux, a macOS and a Windows machine of
  GitHub Actions (`.github/workflows/desktop.yml`) when a release tag is pushed.
