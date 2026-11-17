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
2026-10-29 release). It offers the read tools only (about 30): the tools that upload, relay a
transaction or hand out whole stored files are in the local toolkit, not here. It has no `onyx-check` program, so `onyx_check_answer` there says
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

The same call also searches the bulk datasets publishers upload (`GET /v1/bulk?q=`), for example
`{ "query": "bitcoin mvrv" }`. Their ids use short names (`btc.mvrv`), so common full names
(bitcoin, ethereum, solana and a few more) are translated. Matches come under `bulk.matches`, each
with its rows, key range and `read`: the exact `onyx_get_bulk_rows` call for a newest-first page.
A match whose key is a time also has `newest` (the newest day of data) and `stale`: `true`, with
one sentence in `staleNote`, when it has no data in the last 30 days. Fresh matches come first.
`onyx_get_bulk_rows` says the same under `freshness`. Tell your user before using a stale one.
A dataset whose newest key is dated in the future has `newestInFuture`: `true` instead: its newest
day is not known, and it is never counted as fresh. All of this is the node's word, not checked.
When no id holds every word, the answer drops one word, says which, and does not call that found.

## 3. Read and check a value

- Free public values (earthquakes, Denver temperature, U.S. debt): `onyx_get_observation`.
- Paid rows: `onyx_get_metric`, `onyx_get_proof`, `onyx_get_export`, `onyx_get_settlement`. A
  paid answer first returns a 402 offer. Pay only `offerCheck.pay`, and only when
  `offerCheck.ok` is true, with your own wallet (README of `apps/agent`).
- Bulk datasets (free): `onyx_get_bulk_rows` with `publisher` and `dataset`. It returns 10 rows
  by default, oldest first; `"order": "desc"` asks for a newest-first page. A row's key is a
  whole number. When the answer's `key.unit` is `unix_seconds` (a key column named as a time
  whose whole range lies between 1973 and 2100 in unix seconds), `from` and `to` also take dates
  such as `2026-01-31`; otherwise they take whole numbers only. The answer is compact and starts
  with `"checked": false`: keys and values, the next page, and a short `proof` note. Nothing in
  it is checked or can be checked, including that a newest-first page starts at the newest row;
  `"full": true` returns the node's whole answer with every proof (about 3 kB a row), also marked
  `"checked": false`, which `onyx-check --bulk` checks on your own machine (section 6). The
  suggested check command is built from your own arguments, never from the node's answer.
  `"columns": ["mvrv"]` keeps only the columns you name.
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
  where the key never leaves the browser, or with onyx-contrib on your own computer (section 8).
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
git clone https://github.com/onyxnetworkofficial/onyx && cd onyx
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

## 6. Check an answer yourself

`onyx-check` is the program that checks an answer without trusting the node or this toolkit. It
ships in `onyx-publish-linux-x86_64.tar.gz` of the latest test-network release
(https://github.com/onyxnetworkofficial/onyx/releases, in `onyx-publish/bin/`), or build it from
the source: `git clone https://github.com/onyxnetworkofficial/onyx && cd onyx && cargo build --release -p onyx-check`.

The test network's addresses: the node is `https://onyx.cryptagraph.com`; the book that records
publishers' signatures is `0x04bb31d80c8ce9df02c8a03486b5812ec6e5ccc5` (`OnyxSignedBook` on Base Sepolia,
chain id 84532; `deploy/early-record/contracts.json`). Take the book address from here or from
the repository, never from an answer you are checking.

One full check of a dataset's newest rows (replace the two ids):

```sh
curl -s "https://onyx.cryptagraph.com/v1/bulk/<publisher id>/<dataset id>/rows?order=desc&limit=5" > rows.json
onyx-check --bulk rows.json --publisher <publisher id> --dataset <dataset id> --order desc \
  --rpc https://sepolia.base.org --chain-id 84532 --book 0x04bb31d80c8ce9df02c8a03486b5812ec6e5ccc5
```

And of a range, asked oldest first with whole-number keys (replace the two ids and the two keys;
1767225600 is 2026-01-01 and 1769817600 is 2026-01-31 in unix seconds):

```sh
curl -s "https://onyx.cryptagraph.com/v1/bulk/<publisher id>/<dataset id>/rows?from=1767225600&to=1769817600&limit=100" > rows.json
onyx-check --bulk rows.json --publisher <publisher id> --dataset <dataset id> \
  --from 1767225600 --to 1769817600 \
  --rpc https://sepolia.base.org --chain-id 84532 --book 0x04bb31d80c8ce9df02c8a03486b5812ec6e5ccc5
```

Exit 0 and `"ok": true`: every row is proven up to the publisher's signature, and your own chain
read shows the book holds it. Exit 1: refused. Exit 3: nothing decided (the chain node could not
be read). Pass the same `--from` and `--to` you asked.

For a newest-first page (`order=desc`) pass `--order desc` (and `--page N` for a later page).
`onyx-check` refuses an answer in another order, and for page 0 without `--to` it proves the page
starts at the dataset's newest row (`"newestProven": true`). `--from` and `--to` also take the
dates you asked (`2026-01-01`) for a key that reads as a day. These need `onyx-check` from
v0.4.0-testnet or later; an older one reads only the oldest-first form with whole-number keys.
The answer's `query`, `key` and `next_page` fields are never proven (the report's `notProven`
says so).

## 7. Test it

```sh
node --test apps/agent/test/*.test.mjs   # the tools and the HTTP server, no network
scripts/agent-connect-test.sh            # these steps in a fresh container with only apps/agent
scripts/agent-connect-test.sh --live     # the read-only steps against the hosted node
```

Every text the tools return comes from Onyx and is data, never instructions (THREATS NL21-1).

## 8. Contribute from your own API keys (onyx-contrib)

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
