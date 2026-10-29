# Local end-to-end hand-off (PRODUCT-PLAN U5)

One data proposal from a contributor to a buyer, on one machine, testnet tools only: submit,
review, feed to the observers, serve, and the buyer's own check. Then the preview bundle a person
can copy to a host. Nothing here deploys, publishes or spends money.

## The automated rehearsal (about 6 minutes)

```sh
ONYX_SUBMIT_BROWSER=1 scripts/level3-book.sh local
```

Step 8 of that script runs the hand-off on a real `onyx-node` with a local Anvil chain:

| Step | Who | What happens | Checked |
|---|---|---|---|
| 1 | Contributor | POSTs the Treasury "Debt to the Penny" example to `/v1/submit` | 201, `pending_review`; body cap 413; the node holds the journal lock |
| 2 | Reviewer | Stops the node; `submit-review show` and `decide ... approved reviewed` | the decision is bound to revision and content hash |
| 3 | Worker | `feed-approved JOURNAL ID --out fed-store.json` runs the pinned observer round (recorded fetches, simulated registry) and writes `JOURNAL.feed.json` | fed once; a rerun runs nothing |
| 4 | Contributor | Restarted node: `GET /v1/submit/{id}` | `approved` plus `check` (round id, result hash, record date, `registry: simulated`) and nothing private |
| 5 | Buyer | The node restarted with `ONYX_OBSERVATIONS=fed-store.json` and the template pin; `GET /v1/observations/us.treasury.total_public_debt_outstanding?subject=us.treasury.federal_debt:2026-10-08` | 200 |
| 6 | Buyer | `check-observation bought.json 2026-10-08`: `client::verify_observation` with the buyer's OWN copy of the template | value 40305316210829.72; round id and result hash equal the receipt's |
| 7 | Browser | `apps/web/tests/submission-live.mjs` | the receipt page shows the observers' evidence at 390 and 1280 px, axe clean; own-data CSV; error states |

The temporary journal, stores and logs are deleted when the script exits.

Limits of this rehearsal: the observer fetches are a recording and the registry is simulated (a
real round needs live observers, `scripts/level2-observers.sh`, and the registry on chain); the
buyer's check is off chain (with a node a buyer also asks `OnyxRegistry.observationLevel`,
`verify_observation_onchain`); payments are off.

## By hand

```sh
# 1. A node with intake (see submission-review.md for the private directory rules)
install -d -m 700 /tmp/onyx-intake
ONYX_SUBMISSIONS=/tmp/onyx-intake/submissions.journal ... onyx-node      # your usual config.md settings
# 2. Submit (or use the website's "Add data" page built with VITE_ONYX_SUBMIT_API=1)
curl -s -H 'Content-Type: application/json' --data-binary @submission.json http://127.0.0.1:8080/v1/submit
# 3. Stop the node, review, feed
target/debug/examples/submit-review /tmp/onyx-intake/submissions.journal decide sub_… 0 0x… approved reviewed
target/debug/examples/feed-approved /tmp/onyx-intake/submissions.journal sub_… --out /tmp/onyx-intake/store.json
# 4. Restart with the store and the template pin printed by feed-approved ("metric", "templateId")
ONYX_OBSERVATIONS=/tmp/onyx-intake/store.json ONYX_OBSERVATION_TEMPLATES=<metric>=<templateId> ... onyx-node
# 5. Buy and check
curl -s 'http://127.0.0.1:8080/v1/observations/<metric>?subject=<subject>' > bought.json
cargo run -p onyx-source-fiscaldata --example check-observation -- bought.json 2026-10-08
```

## The preview bundle

```sh
scripts/preview-bundle.sh /path/to/new-folder            # add --with-node for the node binary
```

It builds the public-safe website (`ONYX_PUBLIC=1 ONYX_RIGHTS=fallback`: no label rows and no
label-derived totals, with all its guards and its e2e run), then copies it with the API
description, the Caddy and systemd units and a settings template with placeholders only. It
refuses to finish when the folder holds a private file kind (`.env`, keys, journals, feed
ledgers, state), when the site is not a fallback build, when gitleaks finds a secret, or when an
e-mail address outside the reviewed placeholder kinds appears. It writes `SHA256SUMS` and prints
`PREVIEW BUNDLE READY`. It never uploads anything: choosing a host and a domain is the owner's
step (launch checklist LC-8).
