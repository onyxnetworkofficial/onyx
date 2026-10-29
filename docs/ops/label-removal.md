# Label correction and removal requests

## Scope

How a request to correct or remove a label (or any data naming a party) is handled, end to end:
intake, triage, decision, removal from every place the label lives, reply, and the record. Applies
to label sets, the website (both builds), the history files, the API and the repository. Not legal
advice; a lawyer reviews this before mainnet ([upgrades.md](upgrades.md) gate). Rules it follows:
CLAUDE.md hard limits (labels name entities only; no data whose licence forbids redistribution;
never rewrite history), docs/spec/rights.md rule 4, docs/spec/labels.md R-REMOVE-1.

## Contact

Onyx has no e-mail contact (the project is anonymous, decided 2026-10-25). Two channels, both on
GitHub:

1. **Public:** an issue titled "label correction" in the repository.
2. **Private:** a request that names a person or holds personal data goes through the
   repository's private vulnerability reporting form (the **Security** tab, **Report a
   vulnerability**), with a title that starts with "Removal". Only the maintainers see it.

## Who may ask, and what for

| Requester | Request | Default outcome |
|---|---|---|
| A person who finds an address of theirs, their name or their handle anywhere Onyx publishes | Remove it | Remove, no proof of identity beyond control of the channel used; a person is never a label (hard limit), so no evidence is weighed. One exception, below |
| A person who claims an address the labelled company proved it controls (a label row at proof level 3: the company's own signature over the address, such as a signed PoR row or a BIP-322 attestation, ADR-0015) | Remove it as theirs | Remove only with a BIP-322 proof (`smp` or `ful`, as `onyx_source_btc::Bip322Proof` accepts) by that address over the message `Onyx removal <request id>`. Without one, the request is handled as an entity correction (row above: the company's signature stands until the source is re-read or the company revokes it) and the reply says how to send the proof. Rule decided 2026-10-18 (THREATS L9A-10; the loop decided under its standing policy, the project owner may change it): a claim without proof would let anyone strip a company's signed reserve addresses |
| An entity that is labelled | Correct (wrong entity, wrong kind) or remove an address it does not control | Resolved by evidence (rights.md rule 4): a signed revocation (`onyx_labels::revoke`) or a BIP-322 proof decides at once; otherwise the source the label came from is re-read |
| A data owner (a list or file Onyx imported) | Stop redistribution | Remove every row from that source and add the source to the excluded list until its terms are re-read |
| Anyone else | Report a wrong or personal label | Treated as the matching case above once the facts are checked |

## Steps

1. **Log** (same day). Open an issue "label correction: <request id>" with the date, channel, the
   address or text in question and where it was seen. Never paste a person's name or contact
   details into the issue, a commit or a doc: refer to them by request id. Store the requester's
   contact only in the mailbox.
2. **Acknowledge** (within 2 working days): confirm receipt and the request id.
3. **Find every copy** (within 5 working days). Search for the address (all spellings: bech32
   lower case, base58 as is) and any name or handle:
   - committed label sets: `fixtures/labels/*.json` and their goldens (`fixtures/golden/*labels-v2*`);
   - reserve scans and flows: `fixtures/reserves/`;
   - website data: `apps/web/static/history/`, `apps/web/static/level2/`, `apps/web/static/run/`,
     and every build (`scripts/build-web.sh`, `ONYX_RIGHTS=fallback|cleared ONYX_PUBLIC=1 scripts/build-web.sh`): the preview
     ships label rows inside the recipe inputs (`data/input-*.b64.txt`), the public build ships none;
   - the API's CAS store and served roots (docs/ops/run-api.md);
   - the pool list: `fixtures/pools/pool-allowlist.json` (names, tags, payout addresses);
   - git history: `git log --all -S '<address>' --oneline`.
   The audit scan in docs/audit/2026-10-13-data-privacy.md ("Scans") is the template.
4. **Decide** (within 10 working days; owner: the project owner or the person he names). Person data: always
   remove. Entity correction: decide on the evidence, record the reason. Data-owner request: remove.
5. **Remove** (same day as the decision):
   1. Publish a new label set version without the label (`onyx_labels::version::VersionLog::append`;
      the manifest diff lists it as removed). Earlier versions are never edited (upgrades.md). A
      removal is the one exception to R-IDX-5 (a new set keeps every earlier label).
   2. For a fixture set (`test.*`, never served): regenerate with the converter
      (`scripts/build-label-set-v2.py` or `-v3.py`) after adding the address to its deny list, and
      re-bless what depends on it (goldens, reserve scans, history exports, site pins); the commit
      message names the request id, not the requester.
   3. Old versions holding the label stay in restricted storage only until every root that pins
      them is past its challenge window (R-REMOVE-1), then their CAS blobs are deleted.
   4. Pool list: add the upstream id to `EXCLUDED_POOL_IDS` with a reason class (never the name),
      regenerate `pool-allowlist.json` (`scripts/build-pool-allowlist.py`), rebuild the pool table.
   5. Rebuild and redeploy the website (public build: `ONYX_RIGHTS=fallback|cleared ONYX_PUBLIC=1 scripts/build-web.sh`, the mode that is live, which
      refuses label rows, unlisted addresses and coinbase handles).
   6. Git history is not rewritten (CLAUDE.md). If a past commit holds the data, add a THREATS row
      and ask the project owner to decide (rewrite with force-push, deleting a branch, or keeping the
      repository private); see THREATS L8-H1..H3 for the existing cases.
6. **Reply** (within 2 working days of the removal): what was removed, from where, and the new
   label set version; or, for a refused entity correction, the evidence and how to submit a
   signed revocation.
7. **Record**: close the issue with links to the commit and the new version; add a line to the
   request log in this file (id, date, kind, outcome; no names).

## Request log

| Id | Received | Kind | Outcome |
|---|---|---|---|
| (none yet) | | | |

## Open questions (Needs the project owner)

- Response times above are proposals, not commitments, until the project owner confirms them.
- The proof rule for company-signed addresses (decided by the loop 2026-10-18): the project owner may
  change it.
- Jurisdiction and the legal basis for keeping old versions under R-REMOVE-1 (legal-data-rights.md).
