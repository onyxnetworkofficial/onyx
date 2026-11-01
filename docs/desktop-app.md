# The Onyx desktop app

The Onyx app lets you add data to Onyx from APIs you already have a key for (a market data or
weather service, any JSON API). **Your API keys stay on your computer**, in its keychain. Onyx
receives only the value you choose to send, after you have seen exactly what will be sent.

This is the **test network** (Base Sepolia): no real money is involved.

## 1. Download and install (about 2 minutes)

Open the website's **Download** page (in the menu). The big button gives you the installer for
your computer. Then:

### Windows (10 and 11, 64-bit)

1. Open `onyx-desktop-windows-x64-setup.exe` from your Downloads folder.
2. Windows may show a blue box, "Windows protected your PC". The app is not code-signed yet, so
   Windows does not know it. Click **More info**, then **Run anyway**.
3. Follow the installer. Onyx is then in your Start menu.

(The `.msi` file is the same app for administrators who install with Windows Installer.)

### macOS (11 or later, Apple silicon and Intel)

1. Open `onyx-desktop-macos-universal.dmg` and drag **Onyx** onto **Applications**.
2. The first time, macOS may say it "cannot verify" the app, because it is not signed with an
   Apple developer certificate yet. Open the Applications folder, **right-click** (or
   Control-click) Onyx, choose **Open**, then **Open** again.
3. If there is no Open button: open **System Settings**, **Privacy & Security**, scroll down and
   click **Open Anyway** next to Onyx, then confirm.

You only do this once. (The app is signed "ad hoc", not by a registered Apple developer. If macOS
ever says the app "is damaged", it means the download's quarantine mark blocked it: open
Terminal and run `xattr -dr com.apple.quarantine /Applications/Onyx.app`, then open Onyx again.)

### Linux (64-bit Intel or AMD)

- **AppImage** (any distribution): right-click `onyx-desktop-linux-x86_64.AppImage`,
  **Properties**, **Permissions**, tick **Allow executing file as program** (or run
  `chmod +x onyx-desktop-linux-x86_64.AppImage`), then double-click it.
- **Debian, Ubuntu, Mint**: double-click `onyx-desktop-linux-amd64.deb` to open it in Software
  Install, or run `sudo apt install ./onyx-desktop-linux-amd64.deb`. Onyx is then in your apps.

On Linux the app keeps keys in your desktop's keyring (GNOME Keyring or KWallet) through
`secret-tool`. If the app says "Install libsecret-tools", run
`sudo apt install libsecret-tools` (Debian, Ubuntu) or `sudo dnf install libsecret` (Fedora).

### Check your download (optional)

Each release has a `SHA256SUMS` file. On Windows: `certutil -hashfile <file> SHA256`; on macOS:
`shasum -a 256 <file>`; on Linux: `sha256sum <file>`. The result must equal the file's line in
`SHA256SUMS`.

## 2. Use the app (about 5 minutes the first time)

Open Onyx. It starts its own small helper on your computer and shows one window with five steps:

1. **Check this computer.** Green lines are fine. A line marked "To fix" says the one thing to
   do. If your computer has no keychain the app can use (some Linux setups), open "Use an
   encrypted file for your keys instead" and choose a passphrase of 12 characters or more. The
   app asks for it again each time it starts and never stores it.
2. **Your API providers and keys.** Press "Add a provider" and fill in what the provider's
   documentation says: its API address (starting with `https://`), how it takes the key (a header
   such as `x-api-key`, or a URL parameter such as `api_key`), and the address of its OpenAPI file
   (a JSON file that lists the API's endpoints). Then paste your key into the provider's card and
   press "Store key". The key goes into your keychain and is never shown again; it is only ever
   sent to that provider's own address.
3. **Read a value and contribute it.** Press "Endpoints" on a provider, pick one, fill in its
   parameters and press "Read the value". The value is read on your computer and shown to you;
   nothing goes to Onyx. Click the part of the answer you want, describe it, and press "Check
   with Onyx": Onyx checks it and records nothing. You then see **exactly** what would be sent
   (your key is not in it: the address shows `{api_key}` where the key goes). Only if you tick "I
   have read what will be sent" and press **Send to Onyx** is it sent.
4. **Your contributions.** Everything you sent, with its receipt. "Check status" asks Onyx where
   it stands (a provider's terms are reviewed before anything is published).
5. **Use Onyx from Claude Desktop (optional).** "Connect to Claude Desktop" adds Onyx to Claude
   Desktop's settings. Your other settings stay as they were, and the old settings file is kept
   as a backup next to it. Quit Claude Desktop completely and start it again; Onyx's tools then
   appear in it. Claude can then list your providers' endpoints and propose values, but never sees
   your keys, and you still confirm every contribution.

Closing the window stops the app and its helper.

## 3. What stays on your computer, what is sent

- **Your keys**: in the keychain (Windows: protected for your user account; macOS: the login
  keychain; Linux: GNOME Keyring or KWallet), or in the encrypted file if you chose it. Sent only
  to the provider they belong to.
- **Your providers and past contributions**: a settings folder (`%APPDATA%\onyx-contrib` on
  Windows, `~/.config/onyx-contrib` elsewhere), never a key in plain text.
- **Sent to Onyx, only after you press Send**: the title and description you wrote, the endpoint
  address with `{api_key}` in place of the key, where the value is in the answer, the value, and
  a fingerprint of the provider's answer.

The app's window talks only to the helper on your own computer (address `127.0.0.1`), with a
secret that changes every time it starts; other programs and websites cannot use it.

## 4. Without the desktop app

- **Command line, no Node needed**: the "For developers" part of the Download page has
  `onyx-contrib` as one file per system. Run `onyx-contrib ui` to open the same app in your web
  browser, or see apps/contrib/README.md for every command.
- **From the source**: `node apps/contrib/src/cli.mjs ui` (Node.js 20 or later).

## 5. For builders

- `scripts/desktop-build.sh <out dir> [targets]` builds the single-file programs (Node 24.22.2's
  official binary with onyx-contrib written in; macOS ones only on a Mac) and smoke-tests this
  machine's one (`scripts/desktop-smoke.mjs`).
- The window is apps/desktop (Tauri 2). It needs the system web view's development libraries, so
  it is built by `.github/workflows/desktop.yml` on Windows, macOS and Linux runners when a
  `v*-testnet` tag is pushed (or by hand from the Actions tab, without attaching anything).
- Decisions and alternatives: docs/adr/0061-desktop-app.md. Threats: THREATS.md section CL10.
- Not done yet: code signing (Windows Authenticode, Apple Developer ID and notarization) is the
  owner's decision; until then users follow section 1's extra click.
