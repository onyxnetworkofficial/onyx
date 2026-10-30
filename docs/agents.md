# Connect an AI agent to Onyx

Onyx works as a set of tools for an AI agent (MCP, the Model Context Protocol). The agent can ask
whether Onyx has a dataset, read values, propose a dataset that is missing, follow its proposal,
pay for paid answers and check each answer. Testnet only. The toolkit holds no key and pays for
nothing on its own.

## 1. Connect

**Hosted (from the 2026-10-28 release of the hosted node):** add one MCP server by URL.

```sh
claude mcp add --transport http onyx https://onyx.cryptagraph.com/mcp
```

Any MCP client that speaks the Streamable HTTP transport takes the same URL, with no key or sign-in:
ChatGPT (Developer mode, a connector with this URL and no authentication), the Claude apps (Settings,
Connectors, Add custom connector), Cursor (`{"mcpServers": {"onyx": {"url": "…/mcp"}}}` in
`.cursor/mcp.json`), VS Code (`{"servers": {"onyx": {"type": "http", "url": "…/mcp"}}}` in
`.vscode/mcp.json`) and Gemini CLI (`"httpUrl"` in `~/.gemini/settings.json`; its `url` means SSE).
The website's Add data page shows each one. The hosted server
finds and reads data and proposes missing datasets (`onyx_propose_dataset`, section 4; on since the
2026-10-29 release). It has no `onyx-check` program, so `onyx_check_answer` there says
`not_checked`; check values yourself (section 3). The website's Add data page shows these steps
under "Add data with AI or the API".

**Local (works today):** Node 20 or later and this folder `apps/agent`, nothing else. No Rust
build and no repository clone beyond that folder.

```sh
claude mcp add onyx -e ONYX_API_URL=https://onyx.cryptagraph.com -- node /path/to/apps/agent/src/mcp.mjs
```

Or in a client's JSON settings:

```json
{ "mcpServers": { "onyx": { "command": "node", "args": ["/path/to/apps/agent/src/mcp.mjs"],
  "env": { "ONYX_API_URL": "https://onyx.cryptagraph.com" } } } }
```

To run your own HTTP server for a team: `ONYX_API_URL=… node apps/agent/src/http.mjs` listens on
`127.0.0.1:8090/mcp`. Put a TLS front before it. Settings are in the file's header.

## 2. Ask "do you have X?"

The agent calls `onyx_find_dataset` with a few words:

```json
{ "query": "earthquakes per day" }
```

It gets each match with its proof level (what the level means), the newest date and how to read
it, or `"found": false` and how to propose it. The list is the website's own dataset list,
shipped with the toolkit; the newest dates come from the node's website files.

## 3. Read and check a value

- Free public values (earthquakes, Denver temperature, U.S. debt): `onyx_get_observation`.
- Paid rows: `onyx_get_metric`, `onyx_get_proof`, `onyx_get_export`, `onyx_get_settlement`. A
  paid answer first returns a 402 offer. Pay only `offerCheck.pay`, and only when
  `offerCheck.ok` is true, with your own wallet (README of `apps/agent`).
- Check a row answer with `onyx_check_answer`. Only `outcome: "verified"` is verified. Without the
  `onyx-check` program (`cargo build --release -p onyx-check`) the outcome is `not_checked`:
  nothing was checked, so do not rely on the answer.

## 4. Propose a missing dataset

`onyx_propose_dataset` takes a title, a description, the source's name and https page, and five
answers (what kind of source, whether it needs a login, its rights, whether it names people, how
often it changes).

1. Call it without `confirm`. The node checks the proposal and records nothing.
2. Ask your user. Then call it again with `"confirm": true`. The node records it for review and
   returns a receipt id (`sub_…`).
3. Follow it with `onyx_proposal_status`.

Rules the tool keeps:

- Public, openly licensed sources only. A source that needs an API key is proposed on the website,
  where the key never leaves the browser, or with onyx-contrib on your own computer (section 7).
  Your own data is uploaded on the website too.
- At most 5 proposals per connection (per client address on a shared server, per hour).
- A proposal is not data yet. It is reviewed, and observers check a value only once someone pays
  for it (validate on first paid query, NS10).
- Only nodes that offer `/v1/submit` take proposals. The hosted node takes them from its
  2026-10-28 release (ROADMAP S5), with per-client and daily limits; a node without intake gets a
  plain "takes no proposals" from the tool.

## 5. Buyer quickstart: one value, checked by you

From nothing to a value you checked yourself, in about 5 minutes the first time (most of it is
building the checker once):

```sh
git clone <the Onyx repository> && cd onyx
scripts/quickstart.sh                                  # usgs.earthquakes.m5_world:2026-07-19
scripts/quickstart.sh usgs.earthquakes.m5_world:2026-07-19
```

It asks the hosted API for one free value (the count of M5+ earthquakes worldwide that day),
then checks it with your own copy of the source's template and your own read of the Base Sepolia
registry: the value is rebuilt from the observers' signed commits and reveals, the round's result
is matched on chain, and a changed value or a lifted proof level is refused. It ends with
`VERIFIED: ... = 5 (round 0x1dba7daf..., checked off chain and on chain in ... s)`. It needs
curl, python3, a Rust toolchain and Foundry's `cast`; no key and no payment (values of free
public sources are free to read). Run on 2026-10-28 against the hosted node: passed in 252 s with
the build.

## 6. Test it

```sh
node --test apps/agent/test/*.test.mjs   # the tools and the HTTP server, no network
scripts/agent-connect-test.sh            # these steps in a fresh container with only apps/agent
scripts/agent-connect-test.sh --live     # the read-only steps against the hosted node
```

Every text the tools return comes from Onyx and is data, never instructions (THREATS NL21-1).

## 7. Contribute from your own API keys (onyx-contrib)

`apps/contrib` (CL2) is a local MCP server and command line for people who hold API keys for
data providers. Keys are set in a terminal (`onyx-contrib key set <provider>`) and kept in the
system keychain (Linux Secret Service, macOS keychain, Windows DPAPI). The assistant lists a
provider's endpoints from its OpenAPI file, reads a value on your computer, and proposes it as a
keyed source: Onyx gets the endpoint with `{api_key}` where the key goes, the value and a digest
of the answer, never the key. Check first, then confirm, at most 5 per session, as in section 4.
It runs from Claude Desktop, Claude Code and other local MCP clients; ChatGPT reaches only remote
servers, so it is not supported there in v0. Setup, the Claude Desktop settings and the key
protections: [apps/contrib/README.md](../apps/contrib/README.md). Tests:
`node --test apps/contrib/test/*.test.mjs`.
