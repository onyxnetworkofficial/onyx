# onyx-contrib: contribute data from your API keys, keys stay home

`onyx-contrib` is a small program you run on your own computer. You keep the API keys you
already have (a market data or weather service, any JSON API) in your system's keychain. Your AI
assistant (Claude Desktop, Claude Code, any MCP client) lists what those APIs offer, reads a value
on your computer, and proposes it to Onyx. **The key is never sent to Onyx, never shown to the
assistant and never written to a file of ours.** Onyx gets the endpoint with `{api_key}` where the
key goes, the value it returned, and a digest of the answer.

Test network only (Base Sepolia, no real money). A keyed source is held for a review of the
provider's terms before anything is published, and stays level 4 (a claim) until several key
holders agree (CL4) or a real prover exists (ROADMAP S2).

## 1. Install (2 minutes)

You need Node.js 20 or later and two folders of the Onyx repository: `apps/contrib` and
`apps/agent` next to it (onyx-contrib reuses the agent toolkit's Onyx client and MCP plumbing).
No `npm install`: there are no dependencies.

```sh
node apps/contrib/src/cli.mjs doctor      # shows the vault and whether it works
```

The vault is your system keychain:

| System | Keychain | Needs |
|---|---|---|
| Linux | Secret Service (GNOME Keyring, KWallet) through `secret-tool` | `sudo apt install libsecret-tools` (Debian, Ubuntu) or `dnf install libsecret` |
| macOS | the login keychain through `security` | nothing (built in); a key may not hold `"` or `\` |
| Windows | DPAPI (your user account) through PowerShell; the protected blob is in `%APPDATA%\onyx-contrib\keys` | nothing (built in) |

The key reaches `secret-tool`, `security` or PowerShell on standard input only, never on a command
line (other programs can read command lines). The Windows and macOS paths are written to those
tools' documented behaviour but were tested here only with stand-in scripts (`test/vault.test.mjs`).

No keychain service (a server, some WSL setups)? Opt in to an encrypted file:
`ONYX_CONTRIB_VAULT=file` and `ONYX_CONTRIB_VAULT_PASSPHRASE=<at least 12 characters>`. The file
(`vault.json`, AES-256-GCM, key from scrypt) is useless without the passphrase, but the passphrase
then sits in your MCP client's settings: this protects a copied vault file, not a machine someone
else can read.

Settings live in `~/.config/onyx-contrib` (`%APPDATA%\onyx-contrib` on Windows; change with
`ONYX_CONTRIB_HOME`). `providers.json` there holds names, URLs and how keys are sent. No key.

## 2. Add a provider and its key (1 minute each)

```sh
node apps/contrib/src/cli.mjs provider add prices \
  --base-url https://api.example.com/v3 --header x-api-key \
  --openapi https://api.example.com/openapi.json --docs https://www.example.com/api-terms
node apps/contrib/src/cli.mjs key set prices
```

(`api.example.com` stands for your provider: use its real base URL, key header and OpenAPI file
from its documentation.) `key set` says where the key will be sent (`https://api.example.com`, in
the `x-api-key` header) and asks for it with a hidden prompt. In a script:
`node apps/contrib/src/cli.mjs key set prices < keyfile`.

How the key is sent: `--header <name>` (for `Authorization: Bearer …` add `--prefix Bearer`) or
`--query <name>` (as in `?api_key=…`). The OpenAPI file is JSON (an https URL, or a local file
from the terminal); YAML is not read in v0, convert it once (`yq -o=json file.yaml`).

A key is stored for its provider **and that provider's host**. If the provider is later pointed
at another host, the key is not found there, and `key set` must be run again.

Other commands: `provider list`, `provider remove <name>` (removes its key too), `key remove
<name>`, `endpoints <name> [words]`, `preview <name> <path> --param k=v --pointer /a/b`.

## 3. Connect your assistant

**Claude Code:**

```sh
claude mcp add onyx-contrib -- node /path/to/apps/contrib/src/mcp.mjs
```

**Claude Desktop:** Settings, Developer, Edit Config, then in `claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "onyx-contrib": {
      "command": "node",
      "args": ["/path/to/apps/contrib/src/mcp.mjs"],
      "env": { "ONYX_API_URL": "https://onyx.cryptagraph.com" }
    }
  }
}
```

Any other MCP client that starts a local (stdio) server takes the same command. `ONYX_API_URL`
defaults to `https://onyx.cryptagraph.com`; set it to use another node.

**ChatGPT:** ChatGPT connects only to remote MCP servers over HTTPS (its Developer mode
connectors); it does not start a program on your computer. So today use onyx-contrib from Claude
Desktop, Claude Code or another local MCP client. onyx-contrib v0 has no HTTP server on purpose:
exposing it through a tunnel would let anyone who learns the URL fetch with your keys. (ChatGPT
can still find and propose public datasets through Onyx's own hosted connector, docs/agents.md.)

## 4. Use it

Ask your assistant, for example: "Which endpoints does my prices provider have? Read bitcoin in
USD and contribute it to Onyx."

| Tool | What it does |
|---|---|
| `contrib_providers` | Your providers and whether a key is stored (never the key) |
| `contrib_add_provider` | Add a provider (no key field; it cannot change an existing one) |
| `contrib_endpoints` | The GET endpoints in the provider's OpenAPI file, 40 per page, the key parameter left out |
| `contrib_preview` | Read a value on your computer; any copy of the key in the answer is removed |
| `contrib_contribute` | Propose it: first a check (Onyx records nothing; you see `willSend`), then, after your yes, `checkId` + `confirm: true` sends exactly that. At most 5 per session |
| `contrib_status` | Follow a contribution by its receipt id |

What reaches Onyx is only what the intake already takes (docs/spec/submissions.md): `answers`
with `what: "keyed_api"`, title, description, source name and page, the endpoint as
`endpointTemplate` with `{api_key}` (query keys) or `endpointUrl` (header keys; the description
names the header), the JSON pointer, unit, series id, the sample value as the description's last
line, and the stub proof `{prover: "stub-s2", real: false}` with the Keccak-256 and length of the
answer. The intake takes a data file (`dataPayload`) only for your own data, not for a keyed
source, so none is sent. All intake byte caps are checked before sending (title 120, description
2,000, URLs 512, pointer 256, unit 32 bytes).

## 5. How the key is protected

- Set only in a terminal (hidden prompt or stdin), stored in the system keychain.
- Sent only to the provider's own origin: paths cannot leave it, no redirect is followed, and a
  parameter named like a key (`api_key`, `token`, `x-api-key`, …) is refused.
- Every tool result and log line is filtered for the key and any 10-character piece of it (as
  typed, any letter case, URL-encoded, JSON-escaped, base64 at any alignment, hex) before it
  leaves the server. A provider's answer is filtered before it is cut or listed.
- A proposal holding the key, or a 10-character piece of it, in any of those forms is refused before any request to Onyx, as the
  website's own guard does (`outgoing()` in apps/web/src/lib/intake.ts).
- A key shorter than 12 characters is refused (too short to check outgoing text against).

What it cannot protect: an assistant with its own shell on your computer (Claude Code with command
approval switched off) can read your keychain like any program you run. Keep command approval on.

## 6. Test

```sh
node --test apps/contrib/test/*.test.mjs   # no network: a fake provider and a fake Onyx intake on 127.0.0.1
```

The fake intake runs the website's own intake rules when Node can load TypeScript (Node 22.18 or
later), so a body it accepts is one the real intake accepts. THREATS.md section CL2 lists each
threat and its test.

## 7. Keyed rounds: make a keyed value level 2 with other key holders (CL4)

One key holder's value stays a level-4 claim ("stated by one key holder"). When at least nine
holders of the same provider's keys take seats in its pool, each round draws several of them;
each fetches the value with **its own key, on its own computer**, and an agreeing strict majority
makes it level 2 ("re-observed by key holders", ADR-0054). Only sources whose terms a reviewer
cleared for this (`OK keyed` in docs/ops/data-sources.md) take part; today none is cleared, so
this runs on the local demo only (`scripts/level2-keyed-anvil.sh`, about 6 minutes).

You need the `level2-keyed` tool: `cargo build -p onyx-level2 --example level2-keyed`, then set
`ONYX_CONTRIB_KEYED_BIN` to `target/debug/examples/level2-keyed`.

```sh
# 1. Take a seat (shows the provider's terms; --yes accepts them without a terminal prompt).
node apps/contrib/src/cli.mjs keyed join widgets --manifest sources/<id>/manifest.json \
  --observer-key ~/observer.key [--operator your-org]
# 2. Observe an announced round (your key goes from the vault to the observer on stdin only).
node apps/contrib/src/cli.mjs keyed observe widgets --manifest sources/<id>/manifest.json -- \
  --key-file ~/observer.key --announcement round.json --state ~/.onyx-keyed-state
node apps/contrib/src/cli.mjs keyed seats   # the seats this computer holds
```

From an assistant: `contrib_keyed_join` (shows the terms first; signs and stores the seat only
after your yes), `contrib_keyed_observe` (starts the observer for an announcement) and
`contrib_keyed_status`. No tool takes a key.

What leaves your computer: the seat (your observer address, the source, your declared operator,
the hash of the terms you accepted, the time, a signature) and your observer's commit and reveal
(the value, the window start, a page hash blinded with fresh random bytes). Never the key, a hash
or fingerprint of it, the provider's page, or your exact fetch time (a private random second).
Every seat fetches every round, drawn or not, and commits at the window's end, so the round's
timing does not single you out. What remains: the provider sees which of its keys fetch at round
times, and a provider that answers your key differently on purpose can find your address from
your public reveal (THREATS CL4-R1-4). A value that holds a piece of your key is never sent.
Onyx cannot tell several keys of one person from several people: a seat
needs a bond held 7 days and 5 agreed public rounds, only one seat per declared operator enters a
round, and the odds that a third of the pool colluding wins a draw are shown next to the value.
On the test network bonds have no value. THREATS.md section CL4 lists each threat and its test.
