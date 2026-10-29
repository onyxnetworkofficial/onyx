# Run the query API locally

The demo server settles one round of the BTC fixture shard (blocks 921,080-921,082, full test
label set) with the M4 code, then serves it with `onyx-api` on `127.0.0.1`. Design: ADR-0023,
docs/spec/delivery.md. No network, no real keys, no real payments.

## As a service

For a long-running server (Base Sepolia, a dev chain) use `onyx-node`, not this demo: it reads
its shards, rows and contracts from disk and the chain, settles payments itself or through an
HTTPS facilitator, runs the reporter, and stops cleanly on SIGTERM (requests, then the settles
they started; give it 75 s, deployment.md). Docker and systemd steps,
backups and upgrades: [deployment.md](deployment.md); settings: [config.md](config.md).

```sh
scripts/demo-local.sh --serve --export-node /tmp/onyx-node    # a local chain and its inputs
set -a; . /tmp/onyx-node/node.env; set +a
cargo run --release -p onyx-node                               # serves on ONYX_LISTEN
curl -s "$ONYX_PUBLIC_URL/v1/health"
```

The demo below needs no chain at all.

## Start it
```sh
# Free mode (default port 8402). Prints the shard id and the endpoints.
cargo run --release -p onyx-source-btc --example serve-api -- --dev

# Paid mode: 2,500 micro-USDC per row while fresh (24 h), x402 v2, local mock facilitator.
cargo run --release -p onyx-source-btc --example serve-api -- --dev --paid --port 8402

# Paid mode against a real facilitator over HTTPS (Base Sepolia; see "Live testnet run").
cargo run --release -p onyx-source-btc --example serve-api -- --dev --paid --live-facilitator
```
`--dev` is required for the demo: the fixture shard is pinned to the `test.*` label set, which
the server refuses without it (403 `test_labels`). Never pass it in production.
The first build compiles wasmtime. On the dev box run it under
`systemd-run --user --scope -p MemoryMax=16G`. Stop with Ctrl-C.

## Query it
```sh
curl -s localhost:8402/v1/health
curl -s 'localhost:8402/v1/metrics/btc.entity-flow?subject=entity:okx'
curl -s localhost:8402/v1/settlement/okx-net-flow-921080-921082
curl -s localhost:8402/v1/proofs/1/<shard>/0        # <shard> is printed at start
curl -s 'localhost:8402/v1/proofs/1/<shard>/0?root=<root>'   # cacheable forever
curl -s localhost:8402/v1/export/1/<shard>
```
In free mode each row answer has `root` = the golden root
`0xf912872a71710d45d000e4c2c3771e7f31e51e0fe3d0bad0557d8fedca3d86b2` and a proof. OKX's net
flow is 148,783,915 sats (the row's value; `tests/golden.rs` pins it). Each answer also has a
`verify` bundle; check it with the client in `onyx_api::client` (delivery.md "Verification").
Against a server on a chain the default is
`ClientNode::new(your_rpc, chain_id, settlement).verify(&body, &expect)`: it reads
`getFinalizedRoot` on your own node and refuses a root that is not final there
(`chain_checked: true` on success). `verify_response(&body, &expect, Some(&answer))` does the
same with an answer you read yourself; `None` is refused since M7 loop 3. The demo ledger is
off-chain, so there is no `getFinalizedRoot` to call: use the explicit opt-out
`verify_response_unchecked(&body, &expect)`, whose result says `chain_checked=false (NOT
VERIFIED ON CHAIN …`; nothing has then checked the root against a node you trust (the server's
own root read uses the operator's node; IDX-R5-3).

## Reserve balances (`btc.entity-reserve`, ADR-0034 Amendment 1)
Served only by `onyx-node-btc` (`cargo run -p onyx-reserve-serve --bin onyx-node-btc`), which is
`onyx-node` plus the serving check `btc-entity-reserve-v1`, configured by `ONYX_RESERVE_GATE`:
```json
{"fixtures": "/path/to/onyx/fixtures", "scan": "reserves/scan-971199.json",
 "labels": "labels/test.onyx.entities.btc.v2.json", "crossChecks": "reserves/cross-checks.json",
 "headers": "reserves/headers-969839-971207.json",
 "startHeight": 969838, "recipeId": "0xd0c1331f82f9a743548ca68de3693a2754ae35e6faef7e047a739ba939b49e34"}
```
`headers` (ADR-0037, THREATS RES-4) is a proof-of-work-checked header chain from the first served
block to at least five blocks past the scan (`scripts/build-reserve-headers.py`): the scan, every
shard's last block and any flows file must be on it with 6 confirmations (`MIN_CONFIRMATIONS`),
or the node refuses to start (the scan) or refuses the shard (a stale branch, or not yet deep
enough). A dated label set replaces `labels` with `datedLabels`, `membershipFlows` (the
per-address flows its membership changes need) and `datedHistory` (every earlier version, oldest
first: a version that back-dates a membership is refused); see docs/spec/labels.md rule 4.
(`deploy/reserve-gate.json` is this config for a checkout or the `onyx-node-btc` image, with
`"fixtures": "../fixtures"`; deployment.md.) Every reserve entry of the catalog file carries
`"servingCheck": "btc-entity-reserve-v1"`, but the gate does not rely on it: it claims every shard
with the reserve schema or recipe id, whatever the entry's metric, and plain `onyx-node` refuses
those shards (`onyx_api::gate::KNOWN_GATES`; review RESAPI-R1 F1). The node judges the finalized
reserve shards at start, every `ONYX_GATE_REFRESH_SECS` (default 60) and after indexer polls
(`/v1/health` `servingChecks`, with `rootReadErrors`; a failed root read keeps earlier answers
instead of refusing the chain); a landing shard must end on the scan's block (its tip hash); a shard on any base other than the one derived from
the committed scan and the finalized rows is 403 `serving_check_failed`, and rows of entities
without an independent cross-check (Binance, BitMEX today) are 403 `not_cross_checked`, both
before any 402. Served rows carry `servingCheck`; `Verified::require_cross_checked` refuses an
answer without it. `scripts/demo-local.sh` (step 8) runs this end to end on anvil.

## Level-2 data (ADR-0027 amendment 4)
`onyx-node` also serves level-2 values (re-observed by several bonded observers) from results
store files: `ONYX_OBSERVATIONS=/etc/onyx/debt-to-the-penny.json[,more.json]`, each pinned to the
reviewed registration of its metric with `ONYX_OBSERVATION_TEMPLATES=metric=0x<template id>[,...]`
(the id `OnyxRegistry.metrics(metricKey)` holds; required, L2API-R4-F1). Write one with
the level-2 runner (recorded Treasury round, no network):
```sh
cargo run -p onyx-source-fiscaldata --example level2-round -- --book /var/lib/onyx/treasury-book.json \
  --store /etc/onyx/debt-to-the-penny.json
curl -s "$ONYX_PUBLIC_URL/v1/observations/us.treasury.total_public_debt_outstanding?subject=us.treasury.federal_debt:2026-10-08"
```
The store file must exist. Only on the fact's very first run on a deployment, add `--new-store`
to that one command (REVLOG-R10-3); never keep it in a script: a run refuses it once the chain
shows draws of the fact and the book holds none (REVLOG-R11-3). A store written before its
deployment tag is tagged with `--tag-store`, a separate mode that checks every published draw
against the chain first and then exits (REVLOG-R11-2); it needs the runner's `--book` (locked while
it tags) and refuses a chain that shows draws past both the store's and the book's (REVLOG-R12-1).
The deployment tag and the final-block reads need an RPC with full archive state: the tag
searches the registry's code at past blocks down to its deploy block, and a node that keeps only
recent state stops the run with that message (REVLOG-R12-5).
Every round is re-checked against the store's template at start; a store whose template is not
the pinned one (or, when the store names a registry, not the one registered on chain), a store
that does not verify,
or a source whose rights forbid redistribution, stops the start. Price: the metric's per-row
policy in the catalog file (`pricing`), decaying from the subject's template-fixed decay start
(docs/spec/pricing.md; no round input moves it). Fixed feeds take `as_of`. Check answers
with `client::verify_observation` and the metric's registered template (your own copy).
Every level-2 answer, its 402 body and its `PAYMENT-REQUIRED` offer (`extensions.limits.info.limits`) carry
`limits`: the two known limits a buyer cannot check away yet, REVLOG-1 (the coordinator sees a
reveal's value before it signs its receipt) and REVLOG-5 (sealed rounds; `applies` true on a
sealed `/attempts` answer), stated in docs/spec/delivery.md §Level-2 limits.
Sales are billed to the fact's observation book in `OnyxRevenue`: open it once from the
operator (reporter) key before serving paid level-2 data, or a self-settle would revert and the
request gets 503: no money moves (nothing is sent), but the server treats the outcome as unknown,
so the payer's unit stays spent and its intent counts toward the per-payer cap of 4 until the
authorization expires (THREATS L9A-2; a refund for "nothing was sent" is TODO E1):
```sh
cast send <OnyxRevenue> 'registerObservation(bytes32)' <registryFactKey> --private-key-file ...
```
(In facilitator mode the reporter opens it itself when a receipt is skipped as unregistered.)
Its whole fee goes to the treasury for now (revenue.md R-REV-12).

## Paid mode
1. Call without payment: `402`, error code `payment_required`, and a `PAYMENT-REQUIRED` header
   (base64 JSON). Decode it with `base64 -d` to read `accepts[0]`: scheme `exact`, network
   `eip155:84532`, amount `2500`, asset, `payTo`.
2. Send `PAYMENT-SIGNATURE`: base64 JSON `PaymentPayload` with `accepted` equal to
   `accepts[0]`, `resource` equal to the challenge's `resource` (the exact URL), and an EIP-3009
   `transferWithAuthorization` signed with EIP-712 (domain from `extra.name`, `extra.version`,
   the chain id and `asset`). The mock facilitator accepts only the demo payer, whose test key is
   `keccak256("demo payer (test key, holds nothing)")`; the server prints its address. Build a
   payload with `onyx_api::x402::exact_evm::sign` (see
   `adapters/source-btc/tests/api.rs`, `fresh_btc_data_is_paid_with_x402_and_decays_to_free`).
3. Answer `200` with the row and a `PAYMENT-RESPONSE` header. The identical request again (same
   header, same URL, same client) within 10 minutes returns the same answer without a second
   charge, up to 3 times; the same header on another URL or from another client gets `402`. If
   the request timed out (504) while the settle ran on, the retry gets the stored answer.
   With the settle check (`ApiState::with_settle_check(Arc::new(ChainSettleCheck::new(rpc,
   usdc, revenue)))`; `onyx-node` sets it in facilitator mode), a settle that times out or
   fails is not left unknown at once: the token's `authorizationState` and logs decide (served
   on proof of the transfer, 402 if the authorization paid someone else, the same authorization
   sent once more if it is unused; ADR-0023 Amendment 2). A settle task then runs up to two
   settles, two checks and the 2 s wait before the second check (50 s with the defaults:
   2 x 16 s + 2 x 8 s + 2 s, `settle_task_budget`).

Limits (`ApiConfig`, defaults): 30 payment attempts per client per minute (IPv4 address or
IPv6 /64; 429), 256 requests in flight (503 `overloaded`), 32 facilitator calls in flight (503
`facilitator_busy`), 12 s per request (504), 4 s per facilitator verify (503) and 16 s per settle
(4x, `SETTLE_PATIENCE`), exports up to 100,000 rows and 32 MiB (413; see below for the order), 6 free
exports per client per minute (429 `rate_limited`), 30 free level-2 `/attempts` answers per
client per minute (`free_attempts_per_minute`, 429 `rate_limited`), 60 s to write a response (`send_timeout`),
roots cached 30 s. `ApiState::new` refuses a request deadline under 2.5 times the facilitator
deadline, and a payment network other than Base Sepolia unless `allow_mainnet` is set. Serve with
`into_make_service_with_connect_info::<SocketAddr>()` (the demo does) or every client shares one
rate limit.

Export size and payment order (M7-L48-F3): a free export, or a paid export whose rows are
already in memory, gets 413 before any 402. A paid export whose rows are not loaded yet gets its
402 first; after the payment is verified the rows are loaded and, if too large, the answer is
413, nothing is settled and the payer's unit is given back.

Behind a reverse proxy (M7-L48-F2), every request comes from the proxy's address, so all
clients would share one rate limit. Set `ApiConfig::trusted_proxies` to the proxy's ranges
(serve-api: `--trusted-proxy 10.0.0.0/8`, repeatable). Only for a request whose peer is in
those ranges, the client is the right-most `X-Forwarded-For` address that is not itself a
trusted proxy. An entry may carry a port (`198.51.100.7:51234`, `[2001:db8::5]:443`); the port
is dropped (M7-L50-F4). From any other peer the header is ignored, so a spoofed header buys nothing.
The proxy must append the real peer to `X-Forwarded-For` (Caddy does by default and drops the
client's own value: `deploy/caddy/Caddyfile`, deployment.md "TLS in front"; nginx:
`proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for`). Default: empty, header ignored.

A response holds its in-flight permit until its body is written, in 64 KiB frames, at most
`send_timeout`; after that the permit is released and the next frame fails, which closes the
connection. The server has no socket write timeout of its own: a client that stops reading
keeps its unsent bytes (up to one export) until it disconnects. A public deployment therefore
needs a reverse proxy in front with a send timeout and TLS (THREATS.md M7-L47-F2, M7-7): the
shipped `deploy/caddy/Caddyfile` cuts a whole response at 75 s (`timeouts write`) and does not
buffer it, so a slow reader holds a permit at most `send_timeout` (deployment.md "TLS in front").

Live testnet run (after the M6 deploy): `--live-facilitator` sends verify and settle over HTTPS
(`x402::HyperPost`: hyper with rustls and the webpki roots, HTTPS only, no redirects, answers
up to 64 KiB, feature `http-client` of onyx-api). The URL is the value of
`ONYX_X402_FACILITATOR` when it is set and not empty (for example
`https://facilitator.x402.rs`), else the public x402.org facilitator,
`https://x402.org/facilitator` (`x402::X402_ORG_FACILITATOR`), which supports Base Sepolia and
needs no account or API key (PLAN q-M7-1 a). The URL must be `https://`; plain `http://` is refused at
startup except to exactly localhost, 127.0.0.1 or [::1] (a dev facilitator; M7-L48-F4), and
the transport sends plain http only to those hosts (M7-L50-F3). The server reads the variable at startup
(`x402::facilitator_url_from_env`). This project is testnet-only: the payment network must be
Base Sepolia (`eip155:84532`, `x402::TESTNET_NETWORKS`); any other network is refused at startup
unless `--allow-mainnet` is given (default off; never use it here), and even then has no default
facilitator URL. The demo payer holds nothing, so a real facilitator refuses its payments; a
funded testnet payer is needed. Tests never call a real facilitator.

## Connection limits (AUD-1-F5, AUD-1R-F1)
`onyx-node` cuts a connection that has not sent its request headers within
`ONYX_HEADER_READ_TIMEOUT_SECS`, holds at most `ONYX_MAX_CONNECTIONS` open, and at most
`ONYX_MAX_CONNS_PER_IP` per client (an IPv4 address or an IPv6 /64). When every slot is taken, a
new connection evicts the oldest one that has not sent a request, so a peer opening idle sockets
cannot shut out a client that sends its request at once; only when every slot is mid-request is a
new connection closed. Clients behind `ONYX_TRUSTED_PROXIES` share the proxy's address, so the
proxy is exempt from the per-client cap: give the proxy its own per-client connection limit and
TLS slowloris protection.

## Checks
- `cargo test -p onyx-api` (router, mock data).
- `cargo test -p onyx-source-btc --test api` (real fixture round, golden root, x402, settlement
  vector).
- `cd contracts && forge test --match-contract OnyxApiVectorTest` (Solidity verifies the BTC
  payload and the 13-leaf vector `fixtures/api/multi-leaf-vector.json`).

## Money loop (ADR-0025)
- `payTo` must be the deployed `OnyxRevenue` address; `/v1/health` reports it as `payTo`, so
  miners and clients can check it. Clients pin it before signing (REV-4):
  `client::PayToPin::from_manifest` over the deployment manifest
  (`{"chainId", "revenue", "usdc", "revenueCodeHash"}`, all required, written by the deploy run
  and published with it), then `pin.check_code(&rpc)` against the client's own node (it checks
  `eth_chainId` and the runtime code hash and returns a `VerifiedPin`), then
  `verified.select(&required)` instead of taking the first offered requirement; any other
  `payTo` is refused. Only a checked pin can select (REV-L3-F9).
- Receipts: each settled payment adds one to `ApiState.receipts` (`/v1/health`
  `receiptsPending`). The operator takes up to 64 (`receipts.peek(64)`, `Receipt::to_json`),
  makes sure each shard is registered (`OnyxRevenue.register(round, shard, fields)`, anyone may
  call it once the shard is final), posts them with `credit` from the reporter key, and only after
  the transaction succeeded calls `receipts.commit(&batch)` (REV-L1-F6). `credit` skips a bad
  receipt with `ReceiptSkipped(why)` instead of reverting. Commit with
  `receipts.commit_with_retry(&batch, &skipped)`, passing the receipts skipped with why 2 (the
  payment is not seen as used yet, or was reorged; `Retry::Payment`) or why 4 (shard not
  registered: register it; `Retry::Unregistered`): they go back in the log and wait before the
  next post, the wait doubling per post (30 s and 15 minutes to start, at most 6 hours;
  `RetryPolicy`), at most 8 posts each: about 63.5 minutes of waits for why 2, about 20 hours for
  why 4 and for receipts no event mentions (REV-L2-F5, REV-L3-F3, REV-L4-F5). The reporter below does all of
  this.
- Persistence and the reporter (REV-11): give the state a journal with
  `ApiState::with_receipt_log("<state>/receipts.jsonl", &Binding { chain_id, contract: revenue })`
  (a self-settling server: `SelfSettle::persist("<state>/self-settle")`, then
  `state.receipts = settle.receipts()`; it also keeps the pending settles). Each batch of changes
  is one checksummed line, synced, so a receipt is on disk before the paid answer returns and
  before any waiting retry sees the payment settled (REV-L3-F7); a start refuses a damaged or
  edited journal (a torn last line is cut), a journal of another chain or `OnyxRevenue`, and a
  journal another process holds (`<file>.lock`; REV-L3-F4). Use one state directory per
  deployment and per replica. Run the reporter in the same process:
  `Arc::new(Reporter::for_self_settle(settle)).spawn(Duration::from_secs(30))` (or
  `Reporter::new(sender, revenue, state.receipts.clone())` with the reporter key behind a public
  facilitator; add `.with_registrar(..)` if the operator can build `register` calls). It resolves
  pending settles, checks once that `revenue` holds code (REV-L3-F2), sends up to 64 receipts per
  `credit` without holding the key while it waits, commits a batch only once its transaction is in
  the node's `finalized` block (`with_confirm(Confirm::Safe | Latest)` to change; REV-L3-F8),
  replaces a `credit` unmined after 2 minutes with 25% higher fees (at most 4 times, never above 4
  times the first fee cap, and never above the sender's own fee cap, `ONYX_MAX_FEE_GWEI`;
  AUD-1R-F2), puts back receipts the events skip with why 2 or 4 or never mention,
  and backs off up to 10 minutes after failures. After a crash it posts again: the contract skips
  receipts it already credited (why 1). Keep its `credit`s in flight on disk too:
  `.with_state_file("<state>/credits-in-flight.jsonl")` (REV-L4-F1); a restart then resumes and
  bumps a stuck `credit` at its own nonce instead of sending a new batch behind it. Up to 4
  `credit`s are in flight at once (`with_max_in_flight`), so a batch waiting to be final does not
  hold up the next (REV-L4-F3). An unmined `credit` is handed to the node again on every post;
  past its last bump and 30 minutes (`with_give_up`) it is reported stuck and new batches wait,
  and the key's `Sender` refuses new sends (settles answer 503, nothing sent) while a transaction
  has been unmined for over 60 s (`with_gap_limit`). Then raise the fee cap (`ONYX_MAX_FEE_GWEI`,
  AUD-1R-F3) or clear the nonce by hand. Give the state the reporter with `ApiState::with_reporter(reporter.clone())`. Watch
  `/v1/health`: `receiptLog` (`file`), `receiptsPending`, `receiptsDropped`, `receiptWriteErrors`,
  `pendingSettleWriteErrors` (the facilitator's own journals: pending settles and its receipt log,
  REV-L4-F6), `acceptingPayments`, and `reporter` (`inFlight`, `stuckSince`, `lastError`).
- Settle intents (OPS-L2-F1): give the state `ApiState::with_intent_log("<state>/settle-intents.jsonl",
  &Binding { chain_id, contract: revenue })`; every paid settle is written there before it is made
  (fail closed: if it cannot be written, nothing is settled, the unit comes back, and paid
  requests get 503 until a restart). Run `Arc::new(IntentResolver::new(rpc, usdc, revenue,
  state.intents.clone(), state.receipts.clone())).spawn(every)`: it turns an intent whose settle
  ended unknown (timeout, error, crash, kill, a receipt that was not durable) into a receipt when
  the token's logs show the authorization's own adjacent `Transfer` of the exact amount to
  `revenue` (OPS-L3-F2), removing the intent only once the receipt is durable (OPS-L3-F1); it drops
  it when, at a pinned block past `validBefore` + 10 min by chain time, the token says "unused", the
  use is found sending the value elsewhere, the payer cancelled it (`AuthorizationCanceled`), or,
  used, the authorization's whole span holds no payment of ours after two complete reads on a node
  that serves that pinned block, plus one read extended back toward `validAfter` (OPS-L3-F3,
  OPS-L4-F1/F2). A node that is behind or a node error keeps it.
- **Every intent ends (OPS-L4, the invariant behind the caps).** Whatever the payer does (cancel,
  spend elsewhere, let it expire, make our settle fail), an intent no settle owns becomes a receipt
  or is dropped within `validBefore` + 10 min + a bounded resolver delay, all in chain time. It
  holds because: `validBefore` must lie between `now` + settle budget + 5 s (capped at
  `maxTimeoutSeconds`) and `now + maxTimeoutSeconds` + `authorization_slack` (300 s), so an
  authorization's possible life is bounded; the span searched is computed in chain time from
  `validAfter`/`validBefore`, and new paid requests get 503 `clock_skew` while this server's
  clock is more than 300 s off the chain's (the resolver measures it each pass; logged); each
  pass judges the earliest-expiring intents first, at most 200 intents and 400 log windows. One
  intent per `(payer, nonce)` (409 `payment_in_use` for another request; the identical retry of
  an unknown settle runs the settle check instead, M7L3-R1-1, or, with self-settle, which has no
  such check, gets 503 `payment_pending` with `Retry-After: 30` and is served from the
  resolver's proof once the transfer is proven, SOAK-1); at most 4 unsettled intents per payer (429
  `payer_busy`, retry after 60 s; settles in flight do not count). So filling the 100,000-intent
  log takes that many funded payer addresses each making a payment fail (for example by
  cancelling the authorization, about 71k gas), again every ~15-20 minutes, within the
  per-client payment-attempt limits. `/v1/health` `settles` shows `intentsOpen`, `recovered`,
  `expiredUnused`, `cancelled` (payer-cancelled authorizations: a rising count is the sign of
  this attack), `clockSkewSeconds`, `unknownOutcomes`, `intentWriteErrors`. `onyx-node` wires
  all of this. It needs read access only.
- A failing operator node keeps intents (OPS-L5-F2). The resolver treats every node error as
  "unknown", never as "unused", so a node that keeps failing (down, rate-limited, or not serving
  the pinned block) drops nothing and recovers nothing: `intentsOpen` grows, paid requests slow to
  429/503 as the caps fill, and no fee is lost. A rate-limiting node only slows drops (every 10th
  call failing still drains about 6% of 1,000 expired intents per pass). Cost per expired intent:
  11 to 29 calls (more as the node's head moves past expiry), at most 200 intents and 400 log
  windows per pass. Alert when `intentsOpen` rises for more than a few passes while
  `expiredUnused` and `recovered` stay flat, or when the resolver log shows node errors; fix or
  switch the node (`ONYX_RPC_URL`) and restart: intents survive the restart.
- Smart-wallet payers are refused (OPS-L5-F1). With self-settle, a payer whose address carries
  code (an ERC-1271 wallet or an EIP-7702 delegation) gets `invalid_exact_evm_payload_signature`
  at verify and at settle: its own code would judge the signature, so no simulation can say
  whether the real transaction succeeds.
- Journals need a local disk (REV-L4-F7): the one-writer lock is `flock`, which on NFS with
  `nolock` or `local_lock` only locks within one host. Their checksums catch damage, not deliberate
  edits: keep the state directory writable by the operator only (REV-L4-F4).
- A failed journal write (REV-L3-F5): `receiptWriteErrors` or `pendingSettleWriteErrors` turns
  non-zero, `acceptingPayments` turns false, and every new paid request gets 503
  `receipt_log_unavailable` before anything is verified or charged (replays of settled payments
  are still answered). The receipts recorded since the error are in memory only, so do not
  restart yet: keep the reporter running until `receiptsPending` is 0 (it posts from memory),
  then fix the disk and restart.
- Contributor pass: build the state with `ApiState::with_contributors(ContributorPolicy::new(src))`
  where `src` is `ChainContributors::new(<revenue>, 1_296_000, rpc)` on chain or a
  `StaticContributors` fed by the indexer off-chain. Defaults: free, 60 reads per address per
  minute and 5,000 rows per day, 30 proof checks per client per minute, 600 source lookups per
  minute server-wide (503 `contributor_checks_busy` beyond), proofs at most 120 s old. All of
  these are per replica. Clients sign with `onyx_api::contrib::sign`.
- Dataset prices: `catalog.default_dataset_pricing = Some(DatasetPricing::default())` (2 cents a
  fresh dataset, at least 4 rows' worth); per metric with `set_dataset_pricing`.
