# Reviewing a publisher application (test network)

For the reviewer of the signed book. A registration cannot be undone: the book binds an id to
its origin and a key to its id for good. So every line below is answered before `registerKey`.
Design: ADR-0066; threats: THREATS.md "PUBON"; the applicant's side: [bulk-publish.md](bulk-publish.md).

**What is built (ADR-0066 slices 1 to 3):** the applicant's tools and the offline check
(`onyx-publish apply`, `doctor`, `verify-application`, `request`, `verify-request`), the limits per
publisher on the node (`ONYX_BULK_PUBLISHERS`; docs/ops/run-api.md "Limits per publisher"), the
book read from a chain node (`--rpc`), the reviewer's tool `book-reviewer`, the public record
`deploy/publishers/` with its checker `scripts/check-publishers.py`, the fetch job
`.github/workflows/publisher-application.yml` and the rehearsal on a local chain
`scripts/publisher-onboard-anvil.sh`. **Not built yet:** the website pages and the pointer route
(slice 4). The limits file is not deployed on the hosted node yet: until it is live there, no
outside key is registered on the hosted book (THREATS PUBON-6).

**The tools.** `cargo build --release -p onyx-publish` builds both commands, `onyx-publish` and
`book-reviewer`. The reviewer's key is read from a file only its owner can read
(`--reviewer-key`, mode 0600); it is never printed, never taken from the environment or the
command line, and never goes on the node. `book-reviewer` works on Base Sepolia (84532) and on
a local chain (31337) only.

**Time:** decide within 5 days. An application lives 6 days and a signed report 7.

## 1. Get the files without touching the applicant's host

The applicant opens one issue titled `Publisher application: <publisher id>` with two lines:
its origin and the Keccak of its application file. Never open the origin from your own machine
or from the node (THREATS PUBON-12, PUBON-14): the fetch shows your address to a host the
applicant runs. The job `publisher-application` (`.github/workflows/publisher-application.yml`)
runs on such an issue on a hosted runner, with no secret and a token that can only read. It writes
the issue's text to a file, runs
`onyx-publish verify-application --pointer <that file> --decisions deploy/publishers --save out/artifact`
and keeps `out/` (the report, the exit code and the fetched bytes) as the build artifact
`publisher-application-<issue number>` for 14 days. It does not read the book (a hosted runner has
no chain node of ours), so its report ends with exit 4 at best: **the job decides nothing**. It
does not run the rehearsal either: `scripts/publisher-onboard-anvil.sh` rehearses the whole flow
with a made-up applicant in every check-all run, not this applicant's sample. Where the job cannot
run (a fork without Actions), run the same command on a throwaway hosted machine.

Download the artifact and check the saved bytes again on your own machine, with the book read
from a chain node (the chain node is not the applicant's host):

```sh
onyx-publish verify-application --dir artifact/ --origin <origin> --hash <application keccak> \
  --chain-id 84532 --book 0x04bb31d80c8ce9df02c8a03486b5812ec6e5ccc5 \
  --rpc <chain node> --decisions deploy/publishers > report.json
```

- **`--rpc`** reads every `KeyRegistered` event of the book from `--from-block` to the node's
  finalized block and checks each against the book's state at that block (`keyState`,
  `originOfSource`). What the list then shows as free (the id, the origin, the signer) is read
  again from the book's own mappings, so a scan that started too late decides nothing (exit 3)
  instead of making a taken name look free. The report names the block under `bookRead`.
  **The scan starts at the block the book was deployed in.** The test network's book is pinned
  in the tool (block 52118336, docs/ops/deployment.md): leave `--from-block` out; another block
  is refused. For any other book name `--from-block`; the tool then requires that the address
  held no code one block earlier (`eth_getCode`, so the node must keep old state). A later start
  would hide an earlier look-alike id or overlapping origin, which no mapping read can make up
  for (review S3-R1 M4). A local chain starts at 0.
- **The scan is kept.** The keys read so far go to
  `${ONYX_OPS_DIR:-~/.local/state/onyx}/book-scan/<chain>-<book>.json` (mode 0600; `--scan-cache
  FILE` names another file; a local chain keeps none unless you name one), with the finalized
  block they are valid to and its hash. A later run checks that the node still shows that hash
  (another fork or node is refused: remove the file), reads every kept key's state from the book
  again, and scans only the new blocks. `book-reviewer scan --rpc … --chain-id … --book …` only
  fills the cache; `--scan-pause-ms N` waits after each request.
- **Use a node that answers wide log ranges.** The public node `https://sepolia.base.org` answers
  200 blocks a request and the chain makes about five blocks a second: the tool halves a refused
  range and waits out a rate limit, but the first scan of the book's whole history takes hours
  there (the scan cache makes every later run short). `--logs-range N` sets the first range
  tried (default 10,000; 200 for the public node).
- **Never `--registered` beside `--rpc`**: the tool refuses the pair. With a chain node the list
  comes from the book, never from a file. Without a node, `--registered registered.json` still
  takes a list written by hand from the book's events (`{"publisher", "origin", "signer",
  "state"}` per key; state `active`, `retired`, `compromised` or `suspended`); copy each origin
  exactly as the event has it. Without either, or with an empty list, the tool exits 4 and says
  under `notChecked` that the book was not read: **never approve on exit 4**.
- `--decisions` is the record folder `deploy/publishers/` (one `decision.json` per folder, a
  list or one object). A refused entry holds `{"state": "refused", "signer": "0x...",
  "blocks_signer_until": <unix>}`; an expired one `{"state": "expired", "signer": "0x...",
  "sample_manifest_root": "0x..."}`.
- Exit 0: every check passed, and the book and the records were read. Exit 1: refused;
  `refused.code` and `refused.reason` say why. Exit 2: a wrong option or a wrong list. Exit 3:
  nothing decided (the origin or the chain node did not answer, the origin answered 429 or a
  5xx, the bytes could not be saved, or the log scan started after the book's first key): try
  again later. Exit 4: every check that ran passed, but the book or the records were not read:
  not a decision.
- The report names the application's signer, id and origin only once the signature is that
  signer's. A file with a wrong signature is nobody's: the report repeats nothing of it.

## 2. What the tool checked (a failure is a refusal with its line)

| # | Check | Refusal code |
|---|---|---|
| 1 | The file at the origin hashes to the issue's line; it is at most 4 KiB, has exactly the known fields and no contact text; the signature recovers `signer`; `chain_id` and `book` are this network's; it has not expired | `malformed`, `bad-signature`, `wrong-network`, `expired`, `bad-time` |
| 2 | The keys document names this id and this address and hashes to `signers_document_keccak`. Every fetch is HTTPS, follows no redirect, connects only to a public address and stops at a byte cap | `keys-document`, `origin-unreachable` |
| 3 | The id passes the grammar, the reserved words (`official`, `verified`, `reviewer`, `operator` as a word, `onyx` anywhere) and the look-alike rule; the origin passes its grammar, the shared-host list and the overlap rule; the book shows id, origin and signer free. An id already bound to this origin passes only with a `cosignature` of a key the book shows active and not suspended, and with both keys in the keys document | `name-not-neutral`, `name-taken`, `origin-overlap`, `no-cosignature` |
| 4 | The signer is in no refused record that still blocks it; a key with an expired record carries the same sample | `signer-blocked`, `sample-failed` |
| 5 | The sample is version 1, one shard, at most 8 MiB, fits its schema, hashes to `manifest_root`, carries `Allowed` and the fixed statement, and its report is signed by `signer` (for a second key: by `signer` or the cosigner); a first application's sample was built within the hour before it | `sample-failed` |
| 6 | The rehearsal on a local chain and node (`scripts/publisher-onboard-anvil.sh`, in every check-all run): apply, verify with `--rpc`, approve with the relay, upload, read rows, `onyx-check --bulk` exits 0. It runs the tools on a made-up applicant, not on this applicant's sample | none: a failure there is a bug in the tools, not a refusal |

## 3. What only you can answer

The report repeats these lines under `forThePerson`. Write each answer down; it goes into the
decision.

| # | Line | How |
|---|---|---|
| 7 | **Neutral names.** The id and the origin name no person, company, product or the network. The origin's host is the applicant's own domain, not a host where anyone gets a name | Read both. The word list cannot know every organisation, and the tool's list of shared hosts (`crates/onyx-bulk/data/shared-hosts.txt`) is not complete: when the host is a pages, app, tunnel, storage or dynamic-DNS name the list lacks, refuse with `origin-overlap` and add the host to the list |
| 8 | **Not someone else.** Nothing in the id, origin or `data` reads as a known organisation's voice | An applicant who is that organisation still gets a neutral id |
| 9 | **Rights.** The licence fits the data | `sample.rights` in the report. For data the applicant did not make, re-read the source's terms and quote them (docs/spec/rights.md "Assumptions") |
| 10 | **Entities only.** No row is about one person, also not under a company or project name | Read the sample's rows and `sample.columns` (THREATS SUB-5) |
| 11 | **Not illegal to host** | A plain look at the sample's text columns |
| 12 | **Operator grouping.** Does this publisher belong to a declared operator? | `declareOperator` works only before a source's first key (ADR-0036 amendment 3) |
| 13 | **Limits to grant.** At most what `asks` says; by default 1 GiB and 16 datasets | The node's limits file gets the entry, and the node is restarted, before the key is registered (docs/ops/run-api.md "Limits per publisher") |

## 4. Decide

- **Write the application down when it arrives** (so a late decision shows as a warning of
  `scripts/check-publishers.py`): `book-reviewer record --application artifact/ --origin <origin>
  --hash <application keccak> --state received` with the chain options above and
  `--records deploy/publishers`. It writes `deploy/publishers/application-<16 hex>/decision.json`,
  named by the application's Keccak: no id, no origin and no text of the applicant.
- **Refuse**: answer in the issue with the reason code (`name-not-neutral`, `name-taken`,
  `origin-unreachable`, `origin-overlap`, `sample-failed`, `rights-unclear`, `personal-data`,
  `not-hostable`, `duplicate`, `no-capacity`, `no-cosignature`), then
  `book-reviewer record ... --state refused [--reason <code>]`. When the tool itself refused the
  file, its code is recorded and `--reason` stays out; when the tool passed it and a person
  refuses, `--reason` is required. The record keeps the application's Keccak, the code, the date,
  and the blocked signer. Delete the fetched bytes and the job's artifact.
- **The signer is blocked from the report only.** `book-reviewer record` takes the signer and
  `blocks_signer_until` from its own check of the saved bytes, never from the command line: until
  then that key is refused again, because its refused sample's report could still be relayed
  once the key is registered (the sample's creation time plus 7 days and 300 seconds; the
  application's own time when the refusal came before the sample was read). When the signature
  or the pointer's hash did not pass, the tool **writes no record**: the file may be a copy that
  someone else broke on purpose to get a pending applicant's key blocked (THREATS APPLY-R1 H1).
  Close such an issue with one line.
- **An expiry**: `book-reviewer record ... --state expired` keeps the signer and its sample's
  root; a later application of that key must carry the same sample.
- **Approve**: write `answers.json`: `{"7": "yes", "8": "yes", "9": "yes", "10": "yes", "11":
  "yes", "13": "yes"}`. **Only yes or no is published, never your words**: `yes` says you checked
  the line and it holds; a `no` on any line stops the tool (refuse instead). **Line 12, the
  operator grouping, is not in the file and not in the public record**: whether a publisher
  belongs to a declared operator says who runs it. Keep that answer, the quoted source terms of
  line 9 and any other reasoning in your private notes (review S3-R1 L5). Then:

  ```sh
  book-reviewer approve --application artifact/ --origin <origin> --hash <application keccak> \
    --chain-id 84532 --book 0x04bb31d80c8ce9df02c8a03486b5812ec6e5ccc5 \
    --rpc <chain node> --records deploy/publishers \
    --limits <the node's publishers.json> --max-gib <the node's ONYX_BULK_MAX_GIB> \
    --node <the node's address> --answers answers.json --reviewer-key <key file, mode 0600> --dry-run
  ```

  1. **The dry run** (`--dry-run`) reads everything and prints what a run would do. It sends
     nothing and writes nothing.
  2. **Every run asks the node first** (`--node`, `GET /v1/bulk?listed=<id>`): a node without
     limits per publisher would give the new key the node's one quota, so the tool stops there
     and nothing is written or sent (review S3-R1 M3). The answer shows only whether limits are
     on and whether this one id has an entry.
  3. **The first run** (without `--dry-run`) repeats every check on the saved bytes; on any
     failure it sends and writes nothing and exits with the check's code. An application the
     record holds as refused, withdrawn or expired is never approved. Then it writes the
     publisher's entry into the limits file (1 GiB and 16 datasets unless `--quota-gib` and
     `--datasets` say less than or as much as the application asks) and **stops with exit 6**,
     because the node does not have the entry yet. A grant that does not fit `--max-gib` beside
     the others is refused before anything is written: answer `no-capacity`.
  4. **Put the limits file on the node and restart it** (docs/ops/run-api.md "Limits per
     publisher"; `onyx-node --check-config` first). The node reads the file only at its start,
     and a registered key must never meet a node without its limit (THREATS PUBON-6).
  5. **The same command again** now finds the entry on the node and sends `registerKey` (id,
     address, origin, the keys document's Keccak, the reason `publisher application <keccak>`)
     from the reviewer key, then relays the applicant's signed sample report with `postReport`
     (from `--gas-key` when you name one, else from the reviewer key), then writes
     `deploy/publishers/<id>/application.json` and `decision.json`. A report the book already
     holds counts as done; a sample older than 7 days is not relayed and the record says
     `"not_relayed": "stale"`.
  6. **After a failure, run the same command again.** A key the book holds, active, for this
     application is not registered twice, a recorded report is not relayed twice, a written
     decision is not written twice. A key registered for the id by another decision stops the
     tool, and so does a key the book shows as retired or compromised since: nothing of it is
     relayed or recorded.
  7. `python3 -I scripts/check-publishers.py --write`, then with `--limits <file> --max-gib <N>`
     and, once the registration is final, `--write --rpc <chain node>`: the record, the status list,
     the limits file and the book agree. Until that run the status list says `approved
     (record)`; with the book's registration found it says `approved`. Commit `deploy/publishers/` and
     `apps/web/static/publishers.json`, and answer in the issue.
- **The gas**: the reviewer key pays `registerKey`; the relay is about 250,000 gas.
- **Never** register an id "to keep it safe", register two ids for one application, or send
  test ETH to a publisher (a transfer links the keys on chain).
- Of two valid applications for one id, the first valid one received wins.

## 5. Later requests

A registered publisher signs what it asks for later with `onyx-publish request` (`withdraw`,
`raise`, `key-stolen`, `retire-key`) and puts the file at
`https://<origin>/.well-known/onyx-publisher-request.json`. A plain issue without one changes
nothing (THREATS PUBON-17). Fetch it from a hosted machine first
(`onyx-publish verify-request --origin <origin> --hash <request keccak> --chain-id 84532 --book
<signed book address> --save artifact/`), then act on the saved bytes:

```sh
book-reviewer request --request artifact/ --origin <origin> --hash <request keccak> \
  --chain-id 84532 --book 0x04bb31d80c8ce9df02c8a03486b5812ec6e5ccc5 \
  --rpc <chain node> --records deploy/publishers --dry-run
```

- The tool checks the request as `verify-request` does, with the book read from the chain and
  **the taken sequences read from the record**: every request it takes is written into the
  record with its `sequence`, and a request at or below the last one taken from that key is
  refused. So a request is taken once; the same file sent again within its 6 days is refused.
  (`onyx-publish verify-request --decisions deploy/publishers` reads the same; `--last-sequence
  N` is the hand-kept form.)
- What it does: **`withdraw`** records the named application as withdrawn; it is never approved
  afterwards. **`raise`** needs `--limits`, `--max-gib` and the new limit you grant, `--quota-gib
  N --datasets N`: the ask alone grants nothing (it may be up to 1024 GiB), a grant over the ask
  or below the current limit is refused, and the entry is rewritten only when the sum still
  fits; restart the node. **`retire-key`** sends `retireKey(id, at)` with the report's time and
  **`key-stolen`** `markCompromised`, both from `--reviewer-key` and only when that key is the
  book's reviewer. A key retires itself with its signature alone; **another** key of the id is
  retired only with the second proof a theft needs, a keys document that no longer lists it
  (review S3-R1 L3). **`key-stolen`** sends
  `markCompromised` with the report's time, and only when the id has another active key at the
  book's finalized block (the order below); `--no-new-key` means the publisher ends without one.
- **`withdraw`** names one application by its Keccak; the tool fetches that application and
  requires that the same key signed both. It withdraws that application and nothing else.
- **`raise`** needs an active key of the id.
- **`retire-key`**: the key itself or another active key of the id. The time must be within the
  hour before the request or later. An earlier time would withdraw what the key signed since
  then, on every node, for good: the tool takes it only with the second proof a theft needs,
  a keys document at the origin that no longer lists the key. Call `retireKey(id, at)` with
  the report's `ask.atUnix`, never with a time from the issue's text.

- **A second key** is an application for the same id and origin with a `cosignature` (table
  line 3). The origin alone gives nothing: whoever took over a domain can write the keys
  document, and is still refused.
- **A stolen key** needs both proofs: the request signed by the stolen key, and a keys document
  at the origin that no longer lists it. With one of the two, pause the publisher on the node
  and call nothing on the book: `markCompromised` cannot be taken back. A key that is retired
  already cannot be reported stolen (it is out of the keys document anyway, so the second proof
  would prove nothing).
- **A stolen key with a move to a new key takes two fetches**, because the two files need two
  different keys documents: the new key's application passes only while the document lists
  both keys (the stolen one cosigns), the theft request only once the document no longer lists
  the stolen key. In this order:
  1. First fetch. The origin holds the new key's application (cosigned by the stolen key), the
     theft request, and a keys document that lists both keys. `verify-application` passes;
     `verify-request` is refused with "still lists the key". That is one proof: pause the
     publisher on the node now.
  2. Register the new key (`book-reviewer approve` on its cosigned application), and wait until
     the transaction is final.
  3. Tell the publisher in the issue to take the stolen key out of the keys document.
  4. Second fetch. `verify-request` now passes: both proofs. `book-reviewer request` sends
     `markCompromised` with the report's `ask.sinceUnix`; then lift the pause
     (`book-reviewer resume`). The tool refuses step 4 before step 2 is final: it finds no other
     active key of the id.

  Never the other way round: after `markCompromised` no cosignature of the stolen key counts,
  and a publisher with one key would have no way to a new one.
- **Pause and resume**: `book-reviewer pause --publisher <id> --limits <file> --records
  deploy/publishers [--reason <code>]` sets the entry to `paused` and writes the record; restart
  the node with the file. A wrong option stops it before anything is written. `--now` (a set
  clock) is taken by every `book-reviewer` command on a local chain only. `resume` sets it back. The book is not touched.
- **The only key is lost**: there is no rotation. The data stays served; the publisher starts a
  new id on a new origin.

## What this review does not cover

Only the first dataset is read. Every later version and dataset of an approved publisher is
unreviewed (THREATS PUBON-5). One reviewer key decides; nobody can overrule it (PUBON-8, PUBON-9).
