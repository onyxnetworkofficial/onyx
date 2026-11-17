# Reviewing a publisher application (test network)

For the reviewer of the signed book. A registration cannot be undone: the book binds an id to
its origin and a key to its id for good. So every line below is answered before `registerKey`.
Design: ADR-0066; threats: THREATS.md "PUBON"; the applicant's side: [bulk-publish.md](bulk-publish.md).

**What is built today (ADR-0066 slice 1):** the applicant's tools and the offline check
(`onyx-publish apply`, `doctor`, `verify-application`, `request`, `verify-request`). **Also built (slice 2):** the limits per
publisher on the node (`ONYX_BULK_PUBLISHERS`; docs/ops/run-api.md "Limits per publisher"), not
deployed on the hosted node yet. **Not built yet:** the CI job, the anvil rehearsal,
`book-reviewer approve` and the public record folder (slice 3). Until the limits file is live on
the hosted node, no outside key is registered on the hosted book (THREATS PUBON-6).

**Time:** decide within 5 days. An application lives 6 days and a signed report 7.

## 1. Get the files without touching the applicant's host

The applicant opens one issue titled `Publisher application: <publisher id>` with two lines:
its origin and the Keccak of its application file. Never open the origin from your own machine
or from the node (THREATS PUBON-12, PUBON-14): the fetch shows your address to a host the
applicant runs. Until the CI job of slice 3 exists, run the fetch on a throwaway hosted
machine and keep what it read:

```sh
onyx-publish verify-application --origin <origin> --hash <application keccak> \
  --chain-id 84532 --book <signed book address> \
  --registered registered.json --decisions deploy/publishers --save artifact/ > report.json
```

Then check the saved bytes again on your own machine, offline:

```sh
onyx-publish verify-application --dir artifact/ --origin <origin> --hash <application keccak> \
  --chain-id 84532 --book <signed book address> \
  --registered registered.json --decisions deploy/publishers
```

- `registered.json` is the book's keys, written by hand from the book's `KeyRegistered` events
  and `keyState` until the tool reads the book itself: a JSON list of
  `{"publisher": "...", "origin": "...", "signer": "0x...", "state": "active"}` (state: `active`,
  `retired`, `compromised` or `suspended`). Copy each origin exactly as the event has it: the
  tool stops on an entry that is not in the registered form (a trailing slash, `https://`, upper
  case), because such an entry would hide from every comparison. The report echoes the list's
  key count and Keccak under `registered`; compare the count with the book.
  Without the list, or with an empty one, the tool exits 4 and says under `notChecked` that the
  book was not read: **never approve on exit 4**.
- `--decisions` is one `decision.json`, a list of them, or a folder with one
  `<publisher id>/decision.json` each (the record folder of slice 3). A refused record holds
  `{"state": "refused", "signer": "0x...", "blocks_signer_until": <unix>}`; an expired one
  `{"state": "expired", "signer": "0x...", "sample_manifest_root": "0x..."}`.
- Exit 0: every check passed, and the book and the records were read. Exit 1: refused;
  `refused.code` and `refused.reason` say why. Exit 2: a wrong option or a wrong list. Exit 3:
  nothing decided (the origin did not answer, or answered 429 or a 5xx, or the bytes could not
  be saved): try again later. Exit 4: every check that ran passed, but the book or the records
  were not read (`--registered`, `--decisions`): not a decision.
- The report names the application's signer, id and origin only once the signature is that
  signer's. A file with a wrong signature is nobody's: the report repeats nothing of it.

## 2. What the tool checked (a failure is a refusal with its line)

| # | Check | Refusal code |
|---|---|---|
| 1 | The file at the origin hashes to the issue's line; it is at most 4 KiB, has exactly the known fields and no contact text; the signature recovers `signer`; `chain_id` and `book` are this network's; it has not expired | `malformed`, `bad-signature`, `wrong-network`, `expired`, `bad-time` |
| 2 | The keys document names this id and this address and hashes to `keys_document_keccak`. Every fetch is HTTPS, follows no redirect, connects only to a public address and stops at a byte cap | `keys-document`, `origin-unreachable` |
| 3 | The id passes the grammar, the reserved words (`official`, `verified`, `reviewer`, `operator` as a word, `onyx` anywhere) and the look-alike rule; the origin passes its grammar, the shared-host list and the overlap rule; the book shows id, origin and signer free. An id already bound to this origin passes only with a `cosignature` of a key the book shows active and not suspended, and with both keys in the keys document | `name-not-neutral`, `name-taken`, `origin-overlap`, `no-cosignature` |
| 4 | The signer is in no refused record that still blocks it; a key with an expired record carries the same sample | `signer-blocked`, `sample-failed` |
| 5 | The sample is version 1, one shard, at most 8 MiB, fits its schema, hashes to `manifest_root`, carries `Allowed` and the fixed statement, and its report is signed by `signer` (for a second key: by `signer` or the cosigner); a first application's sample was built within the hour before it | `sample-failed` |
| 6 | The rehearsal on a local chain and node | not built (slice 3) |

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

- **Refuse**: answer in the issue with the reason code (`name-not-neutral`, `name-taken`,
  `origin-unreachable`, `origin-overlap`, `sample-failed`, `rights-unclear`, `personal-data`,
  `not-hostable`, `duplicate`, `no-capacity`, `no-cosignature`). Keep of the application only its
  Keccak, the code and the date. Delete the fetched bytes.
- **Block the signer only from the report.** Every report has `blocksSignerUntil`. When it is
  a number, also keep the report's `signer` and that number as `blocks_signer_until`: until
  then that key is refused again, because its refused sample's report could still be relayed
  once the key is registered (the sample's creation time plus 7 days and 300 seconds; the
  application's own time when the refusal came before the sample was read). When it is `null`,
  **record no signer and no block**: the signature or the pointer's hash did not pass, so the
  file may be a copy that someone else broke on purpose to get a pending applicant's key
  blocked (THREATS APPLY-R1 H1). Such a pointer gets no record at all; its issue is closed with
  one line. Never type a signer or a time by hand from the application file.
- **Approve**: only with a report that read the book and the records (`notChecked` names
  neither), and only once the node lists the publisher's limit. Register the key with
  the reviewer's key, which never goes on the node:
  `cast send <book> 'registerKey(string,address,string,bytes32,string)' <publisher id> <signer> <origin> <keysDocumentKeccak> "publisher application <application keccak>"`.
  Then tell the applicant in the issue to anchor and upload its sample folder (steps 9 and 10 of
  the guide); with slice 3 the reviewer's tool relays the sample's report in the same run.
- **Never** register an id "to keep it safe", register two ids for one application, or send
  test ETH to a publisher (a transfer links the keys on chain).
- Of two valid applications for one id, the first valid one received wins.

## 5. Later requests

A registered publisher signs what it asks for later with `onyx-publish request` (`withdraw`,
`raise`, `key-stolen`, `retire-key`) and puts the file at
`https://<origin>/.well-known/onyx-publisher-request.json`. A plain issue without one changes
nothing (THREATS PUBON-17). Check it as you check an application, from a hosted machine first:

```sh
onyx-publish verify-request --origin <origin> --hash <request keccak> \
  --chain-id 84532 --book <signed book address> \
  --registered registered.json --last-sequence <N> --save artifact/ > report.json
```

- `--last-sequence` is the highest `sequence` you already took from this signer (0 for none).
  Write the report's `sequence` down with every request you act on: a request is taken once,
  and the same file sent again within its 6 days is refused. Without the option the tool exits 4.
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
  2. Register the new key (`registerKey`), and wait until the transaction is final.
  3. Tell the publisher in the issue to take the stolen key out of the keys document.
  4. Second fetch. `verify-request` now passes: both proofs. Call `markCompromised` with the
     report's `ask.sinceUnix`, then lift the pause.

  Never the other way round: after `markCompromised` no cosignature of the stolen key counts,
  and a publisher with one key would have no way to a new one.
- **The only key is lost**: there is no rotation. The data stays served; the publisher starts a
  new id on a new origin.

## What this review does not cover

Only the first dataset is read. Every later version and dataset of an approved publisher is
unreviewed (THREATS PUBON-5). One reviewer key decides; nobody can overrule it (PUBON-8, PUBON-9).
