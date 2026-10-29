# Runbook: deploy OnyxSignedBook and pin it (ROADMAP C10, lead only)

**Trigger:** the lead decides to put the level-3 chain book on Base Sepolia (ROADMAP C10's deploy
step), or to replace it (a reviewer key that leaked: the reviewer is immutable, so the fix is a
new book).

**What it is:** `OnyxSignedBook` (ADR-0028 amendments 6 and 7) is a contract of its own. It has
no link to the settlement, so it is deployed next to the running ledger: no new ledger id, no node
state move. The node's manifest gains three fields (`signedBook`, `signedBookCodeHash`,
`signedBookReviewer`); the node checks them at start and serves `/v1/signed/{fact}` only against
`settleable` at its finalized block (docs/spec/proof-ladder.md R-SIG-ON-7).

**Money:** none. Nothing on chain reads the book for money; the API's level-3 route is free and no
market settles on it (`/v1/settlement` serves catalog shards only). Level-3 money settlement waits
for ROADMAP H6.

**Who:** the project owner runs every step with the deployer key in `.env` (never printed). The reviewer is
a **different** key: the script, the contract's constructor and the node all refuse the deployer
(the settlement's coordinator) as reviewer (THREATS C10-1). On testnet the reviewer is one key;
before mainnet it becomes a multisig or timelock (ROADMAP H1). **Time:** about 15 minutes.
**Cost:** one contract creation; the dry run prints forge's estimate (2026-10-17 rehearsal on a
Base Sepolia fork: 3,344,188 gas, `Estimated amount required: 0.000036271350648168 ETH` at that
day's fees). Test ETH only.

## Preconditions

1. A checkout of the commit to deploy, `forge` and `cast` in `~/.foundry/bin`, `cargo` (the pin
   step builds the `pin-signed-book` example).
2. `.env` holds `BASE_SEPOLIA_PRIVATE_KEY` (the deployer, = the settlement's coordinator) and,
   optionally, `BASE_SEPOLIA_RPC_URL`.
3. The reviewer key exists, apart from the deployer: a fresh wallet used only for reviewing
   sources, kept offline except to sign `registerKey` and the other reviewer actions. Note only
   its **address** here: `REVIEWER=0x...`. Fund it with a little test ETH for those actions.
4. The node's current manifest, copied out of `/etc/onyx` (the script edits only its copy):

   ```sh
   mkdir -p ops-store/signed-book
   sudo cp /etc/onyx/deployment.json ops-store/signed-book/deployment.before.json
   cp ops-store/signed-book/deployment.before.json ops-store/signed-book/deployment.json
   ```

## Steps

1. Plan (simulates the deploy; sends nothing):

   ```sh
   scripts/deploy-signed-book.sh testnet --reviewer "$REVIEWER" \
     --manifest ops-store/signed-book/deployment.json --plan
   ```

   Expected: `chain 84532 (testnet); reviewer 0x...; manifest ...`, `1. dry run (simulation;
   nothing sent)`, the `Estimated total gas used for script` and `Estimated amount required`
   lines, then `PLAN ONLY: nothing sent; rerun with --yes to deploy`. A reviewer equal to the
   settlement's `coordinator()` (the deployer, here) stops before the dry run with `the reviewer
   is the settlement's coordinator 0x...: refused (THREATS C10-1); nothing sent`; a reviewer
   equal to the deploy key in any other setup stops at `dry run refused: nothing sent`
   (`ReviewerIsDeployer`). The pin tool is built before anything is sent. The rehearsal runs this
   same `--plan` path on anvil and checks the deployer's nonce and the manifest did not change
   (review C10-R2-3).

2. Deploy and pin:

   ```sh
   scripts/deploy-signed-book.sh testnet --reviewer "$REVIEWER" \
     --manifest ops-store/signed-book/deployment.json --yes
   ```

   Expected, in order: `2. deploy`, `OnyxSignedBook 0x<BOOK> (tx 0x...)`, `broadcast kept at
   ops-store/signed-book/deployment.json.signed-book-broadcast.json`, `3. pin into the
   manifest`, the lines `signedBook 0x<BOOK>`, `signedBookCodeHash 0x...`,
   `signedBookReviewer 0x<REVIEWER>`, `coordinator 0x<deployer> (not the reviewer)`,
   `PINNED ...`, then `4. read back` with `reviewer()` = `REVIEWER`, the same code hash and
   `DISPUTE_WINDOW_SECS 3600`, and last `SIGNED BOOK DEPLOYED AND PINNED`. The pin step refuses
   (and leaves the manifest unchanged) if the code at the address is not this checkout's build
   with only `reviewer` filled in, if `reviewer()` is not `REVIEWER`, or if `REVIEWER` is the
   settlement's coordinator. If the pin step fails after the deploy (`pin failed; the book is
   deployed`), do not deploy again: fix the cause (an RPC that timed out, a wrong `--reviewer`)
   and resume without sending anything:

   ```sh
   scripts/deploy-signed-book.sh testnet --reviewer "$REVIEWER" \
     --manifest ops-store/signed-book/deployment.json \
     --pin-only ops-store/signed-book/deployment.json.signed-book-broadcast.json
   ```

   Expected: `pin only (nothing sent)`, the four pin lines, `SIGNED BOOK PINNED`. `--pin-only`
   never reads the deployer key, and a rerun on a manifest that already pins the same book
   writes the same three fields again.

   If the deploy step itself fails after forge wrote a broadcast (`deploy failed after forge
   wrote a broadcast, kept at ...`), the creation may have been sent: look the book up on the
   explorer (the deployer's latest transaction) before any rerun; if it is there, resume with
   `--pin-only` on the kept file, never `--yes` again.

3. Check on chain from any shell (no secrets):

   ```sh
   RPC=https://sepolia.base.org; BOOK=0x<BOOK>
   cast call "$BOOK" 'reviewer()(address)' --rpc-url "$RPC"        # REVIEWER
   cast codehash "$BOOK" --rpc-url "$RPC"                            # signedBookCodeHash
   cast codesize "$BOOK" --rpc-url "$RPC"                            # at most 24576
   ```

4. Install the new manifest and restart the node:

   ```sh
   sudo install -m 0644 ops-store/signed-book/deployment.json /etc/onyx/deployment.json
   sudo systemctl restart onyx-node
   journalctl -u onyx-node -n 20 --no-pager | grep 'signed book'
   ```

   Expected: `onyx-node: signed book 0x<BOOK> pinned (code hash and reviewer checked): level 3
   is served only against settleable at a finalized block`. No `/v1/signed` route is mounted
   until a level-3 coordinator writes its post log and `ONYX_SIGNED_REPORTS` names it (no real
   source is wired yet, THREATS L3-F8).

5. Record `BOOK`, `REVIEWER` and the deploy tx in [deployment.md](../deployment.md) "Deployed
   addresses" and the launch checklist's key map (section 2), in one commit.

## Verify

6. `curl -s http://127.0.0.1:8080/v1/health` answers 200 (the node started with the pin).
7. A manifest with any of the three fields changed must not start the node (the rehearsal does
   this on every run: `scripts/level3-book.sh fork`, step 4).

## Level 3 stopped (`level3_posts_delayed`, `level3_clock_skew`)

Not part of the deploy: once a level-3 coordinator posts (ROADMAP C60), every `/v1/signed`
answer stops while any of its posts has been pending more than 600 s, on purpose (spec
R-SIG-ON-7 (c): a sequencer holding one post back may be holding others). It clears by itself
when the post lands. The health watch alerts before it trips (ROADMAP C62:
`node.level3.pending_for` at 300 s, from `/v1/health`'s `level3.oldestPendingSecs`), and again
once it has (`node.level3.stopped`).

1. Find the pending entries: in the file `ONYX_SIGNED_REPORTS` names, the reports whose
   `chain.status` is `pending`, and their `chain.error`.
2. A transport error (RPC down, out of gas funds, fee too low): fix it (fund the coordinator's
   account, switch RPC); the coordinator's `retry_pending` lands the post and level 3 resumes.
3. No error but nothing lands for an hour or more: treat it as sequencer censorship. Do not
   edit the log; level 3 stays off (that is the protection). Note it in deployment.md and wait
   for forced inclusion (hours, ROADMAP H6).
4. An error that can never clear (the same deterministic failure on every retry; payloads over
   4,096 bytes are refused before posting, review C10-R2-1): only then set that entry's `chain`
   to `{"status": "refused", "error": "OperatorDropped"}` and save the file. This lifts the hold
   on that report's fact too, so never do it for a report that contradicts a served value.

`level3_clock_skew`: this server's clock is more than 300 s behind the coordinator's. Fix NTP on
both hosts (`timedatectl`); answers resume by themselves.

## Rollback

The book holds nothing the node depends on until a coordinator posts to it. To undo:

```sh
# If ONYX_SIGNED_REPORTS was set since, remove it from /etc/onyx/node.env first: with it and no
# signedBook in the manifest the node refuses to start.
sudo install -m 0644 ops-store/signed-book/deployment.before.json /etc/onyx/deployment.json
sudo systemctl restart onyx-node
journalctl -u onyx-node -n 20 --no-pager | grep -c 'signed book'   # 0
```

The contract stays on chain unused (no proxy, no pause, no selfdestruct). If the reviewer key
leaked, never register a key in that book again and deploy a new one with a new reviewer
(`--replace` lets the pin step overwrite the old `signedBook`); say so in deployment.md.

## Rehearsal

`scripts/level3-book.sh local` (in scripts/check-all.sh) and `scripts/level3-book.sh fork` (an
anvil fork of Base Sepolia) run the same script in `anvil` mode with anvil dev keys: settlement
deployed and its manifest written, the deployer as reviewer refused with the manifest unchanged,
deploy and pin, three broken pins refused by onyx-node, then the node serving level 3 to the
coordinator's posts. Both passed on 2026-10-17 (branch signed-wiring; book code hash
`0x9b4da615f513b46d12684cc596fc400882aa9ef975acd170d4296ae4cae79507` on both chains). The `testnet`
mode itself has never been run by the loop (lead only).
