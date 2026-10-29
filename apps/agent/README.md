# Onyx agent toolkit

Find, pay for and check Onyx data from an AI agent or a script (ADR-0047). Testnet only.

Quickstart for AI agents: [docs/agents.md](../../docs/agents.md).

- `src/client.mjs`: an API client built from `openapi.json` (one call per operation).
- `src/mcp.mjs`: an MCP server (stdio) with one tool per operation, `onyx_check_answer` and the
  agent-connect tools of `src/connect.mjs`: `onyx_find_dataset` ("does Onyx have X?"),
  `onyx_propose_dataset` (check, then record on confirm) and `onyx_proposal_status`.
- `src/http.mjs`: the same tools over HTTP (MCP Streamable HTTP, one URL), read-only by default,
  rate-limited per client address, sessions by `Mcp-Session-Id`, foreign browser origins refused.
- `openapi.json` and `catalog.json`: copies of `docs/api/openapi.json` and the website's dataset
  list, so this folder runs on its own. `node scripts/sync.mjs` writes them; `--check`, the tests
  and `scripts/check-openapi.py` fail on a stale copy.
- `src/check.mjs`: runs `onyx-check`, the Rust command that checks an answer
  (`crates/onyx-check`). No check is written in JavaScript.

Node 20 or later. No npm dependencies.

## Build the checker

```sh
cargo build --release -p onyx-check        # target/release/onyx-check
```

## Use it from an MCP client

Add a server entry like this to your MCP client's settings:

```json
{
  "mcpServers": {
    "onyx": {
      "command": "node",
      "args": ["/path/to/onyx/apps/agent/src/mcp.mjs"],
      "env": {
        "ONYX_API_URL": "http://127.0.0.1:8402",
        "ONYX_CHECK_BIN": "/path/to/onyx/target/release/onyx-check",
        "ONYX_CHECK_RPC": "https://sepolia.base.org",
        "ONYX_CHECK_CHAIN_ID": "84532",
        "ONYX_CHECK_CONTRACT": "0x…your trusted settlement contract…"
      }
    }
  }
}
```

| Setting | Meaning |
|---|---|
| `ONYX_API_URL` | The Onyx node to ask. HTTPS, or http to this machine. |
| `ONYX_CHECK_RPC`, `ONYX_CHECK_CHAIN_ID`, `ONYX_CHECK_CONTRACT` | Your own chain node and the settlement contract you trust. The root is read there. |
| `ONYX_PAY_MANIFEST`, `ONYX_PAY_MAX_AMOUNT` | The deployment manifest you trust (`{"chainId","revenue","usdc","revenueCodeHash"}`) and your limit per answer in the token's smallest unit. Every 402 offer is then checked on `ONYX_CHECK_RPC`. |
| `ONYX_CHECK_UNCHECKED=1` | Allow checks without a chain node. The outcome is then `unverified_on_chain`. Demo only. |
| `ONYX_MCP_ALLOW_SUBMIT=1` | Add the raw `onyx_submit_proposal` (stdio only). `onyx_propose_dataset` is the checked way. |
| `ONYX_MCP_READ_ONLY=1` | Leave out `onyx_propose_dataset` (stdio; the HTTP server leaves it out unless `ONYX_MCP_ALLOW_PROPOSE=1`). |

## What an agent does

1. Ask: `onyx_get_proof`, `onyx_get_settlement`, `onyx_get_metric`, `onyx_get_export`, and the others.
2. Pay, if the answer is a 402: the result has the x402 offer in `paymentRequired` and the
   check of that offer in `offerCheck`. Pay only `offerCheck.pay`, and only when
   `offerCheck.ok` is true. Sign with your own wallet and call again with `PAYMENT-SIGNATURE`.
   The toolkit holds no key.
3. Check: `onyx_check_answer` with the result's `answerId` and what you asked, for example
   `{"kind":"proof","round":1,"shard":"0x…","index":0}`. Only `outcome: "verified"` is
   verified. Take the question from your own sources (the market contract, the metric
   registry), never from the answer.

## Use it from a script

```js
import { createClient } from './src/client.mjs';
import { checkAnswer } from './src/check.mjs';

const onyx = createClient({ baseUrl: 'http://127.0.0.1:8402' });
const r = await onyx.call('getProof', { round: '1', shard, index: '0' });
const c = await checkAnswer({ answer: r.body, asked: { kind: 'proof', round: 1, shard, index: 0 },
  node: { rpc: 'https://sepolia.base.org', chainId: 84532, contract } });
// c.outcome: 'verified' | 'unverified_on_chain' | 'refused' | 'undecided'
```

## Command line

```sh
onyx-check --answer answer.json --asked @asked.json --rpc <url> --chain-id 84532 --contract 0x…
onyx-check --answer - --asked '{"kind":"export","round":1,"shard":"0x…"}' --unchecked
onyx-check --offer offer.json --manifest deploy.json --max-amount 10000 --rpc <url>
```

Exit 0 verified (or payable), 1 refused, 2 usage error, 3 nothing decided (your node could not
be read, or your settings are wrong).

## Tests

```sh
node --test apps/agent/test/*.test.mjs     # no network; the live part skips
scripts/agent-smoke.sh                     # with two demo servers: ask, check, refuse, 402
scripts/agent-connect-test.sh              # docs/agents.md in a fresh container (only this folder)
scripts/agent-connect-test.sh --live       # its read-only steps against the hosted node
```

Not checked yet by `onyx-check`: level-2 observations, signed facts, track records and salted
label rows (ADR-0047, Consequences).
