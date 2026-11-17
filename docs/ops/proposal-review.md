# Reviewing proposals (test network)

For the reviewer of the proposal queue. A visitor can propose a dataset on the website (Add
data), with `POST /v1/submit`, or through the AI connector. The node writes each proposal to its
intake journal and publishes nothing by itself. This page is the reviewer's tool, the checklist
and the daily routine. Design: ADR-0039 amendment 3; threats: THREATS.md "REV-Q"; rules:
[submissions.md](../spec/submissions.md), [rights.md](../spec/rights.md).

**What is built:** the tool `onyx-node proposals` (list, show, clear, hold, drop), the operator
script `scripts/proposal-review.sh` that runs it on the server, the checklist as a required input
of `clear`, the three new states on the proposer's receipt, and a warning in the node's log from
80% of the journal's cap. **Not built yet:** see "What is not built" at the end. Clearing a
proposal verifies and publishes nothing.

## The tool

```sh
scripts/proposal-review.sh list            # the proposals waiting for a decision, oldest first
scripts/proposal-review.sh list --all      # the decided ones too
scripts/proposal-review.sh show sub_…      # one proposal whole (private: for the reviewer only)
scripts/proposal-review.sh clear sub_… --terms-reread yes --no-personal-data yes \
    --entities-only yes --source-public yes --rights-level allowed
scripts/proposal-review.sh hold sub_… --reason unreachable_source
scripts/proposal-review.sh drop sub_… --reason terms_forbid
```

The script runs `onyx-node proposals …` on the server over `ssh` (`ONYX_REVIEW_HOST`, default
the site's host; `local` runs it on this machine as you). On the server itself:
`sudo onyx-node proposals list --journal /var/lib/onyx/state/submissions.journal`.

- **Never as root on the state directory** (THREATS REVQ-R1-H1). The node's own user owns that
  directory, so a root process there could be led through a planted link. The script's root
  step (`sudo -n sh`) only goes into the journal's folder, reads who owns the journal's name
  without following it, refuses a link or a file of root, and starts the tool as that owner with
  `setpriv`, no groups and no new privileges. The owner is read on every run: the node runs
  under a user systemd makes up. The root step sets its own `PATH` first, so it never runs a
  program from the node's folder, and it refuses a journal that has a second name (a hard link)
  or that does not belong to the owner of its folder: either could be another user's file.
  Started as root by hand, the tool does the same itself before it opens anything. The tool
  also closes itself to tracing by other processes of its user. It needs `sudo -n` for `sh` and
  `setpriv` (util-linux) on the server.
- The tool refuses a journal, a decisions journal or a lock that is a symbolic link or not a
  regular file, and never follows one.

- **The node keeps running.** The tool asks the node nothing. It reads the intake journal
  without taking its lock and never writes it. Each decision is one line appended to a second
  journal beside it, `submissions.decisions.journal`, in the same checksummed format with its
  own lock. Nothing already written is ever changed or removed. The node reads the new line the
  next time someone asks for that receipt; no restart.
- **Who may decide:** whoever can write the node's state directory. There is no reviewer
  account, no password and no route on the node. The journal keeps no reviewer name.
- **`list`** prints how full the journal is, then one line per proposal: the receipt id the
  proposer holds, the state, the age, the source's host and the proposer's rights answers. No
  title, description or sample.
- **`show`** prints every field of one proposal. Never paste its output anywhere public.
- A proposal's text is a stranger's. A word shaped like an e-mail address or a URL that carries
  a secret prints as `[withheld]` (the rest of the field is shown, and the command does not
  fail); control characters and invisible or direction-changing characters print as `\u{..}`,
  so nothing can hide or reorder what you read. The intake itself still takes such text in a
  description: its rules are shared with the website through one vector file, and changing them
  is a separate item.
- Exit codes: 0 done; 1 a journal could not be read or written, a name is a link or belongs to
  root, or the server was not reached;
  2 a usage error; 3 the decision was refused and nothing was written (unknown id, already
  decided, an incomplete checklist, a reason outside the list).

## States and reasons

| Decision | The proposer's receipt says | After it |
|---|---|---|
| none yet | "Waiting for review" (or "On hold" when the intake itself held the source) | `clear`, `hold` or `drop` |
| `clear` | "A reviewer read your proposal and cleared it for the next step. Your data has not been verified or published yet." | `drop` only (a wrong clear is undone by a drop) |
| `hold` | "A reviewer put your proposal on hold", with the reason | `clear` or `drop` |
| `drop` | "A reviewer decided this proposal will not proceed", with the reason | nothing: final. The proposer can send a corrected proposal |

A proposal the intake held by itself (`held_rights_review`, `held_keyed_source_review`) cannot be
cleared with this tool (ADR-0039 item 4); it can be put on hold or dropped.

`hold` and `drop` need a reason code from this list. The proposer sees the code's sentence, never
a text of yours:

| Code | When |
|---|---|
| `terms_forbid` | The source's terms do not allow redistribution |
| `personal_data` | A field, the sample or the source holds data about a person |
| `not_entities` | The subject is a person, not an organisation, a fund or a protocol |
| `duplicate` | The same data was proposed before or is already on Onyx |
| `unreachable_source` | You could not open the source |
| `out_of_scope` | Not a kind of data Onyx takes now |
| `spam` | It describes no real data |
| `other:CODE` | Anything else. `CODE` is 1 to 32 of `a-z`, `0-9` and `_`, for your own records; the proposer sees "another reason" only |

## The checklist

`clear` is refused unless every line is answered `yes` and a rights level is given. Answer from
what you checked today, not from the proposer's answers: those are advice and can be false.

1. **`--terms-reread yes`: I re-read the source's terms today and they allow redistribution**
   (THREATS SUB-3; [data-sources.md](data-sources.md) section 5). Open the terms page of the
   source itself. No terms found, or terms that restrict reuse you cannot meet: `drop` with
   `terms_forbid`, or `hold` while you find out.
2. **`--no-personal-data yes`: no field, sample or source page names or describes a person**
   (SUB-4). Read the title, the description, the sample and the page the source URL opens. The
   intake catches e-mail shapes only; names and phone numbers are yours to catch. Any: `drop`
   with `personal_data`.
3. **`--entities-only yes`: the data is about entities** (SUB-5; rights.md rule 2). An exchange,
   a fund, an agency, a protocol. Not one person under a company or project name (a one-person
   company, a wallet called a fund). If so: `drop` with `not_entities`.
4. **`--source-public yes`: the source is public and I reached it today.** No login, key or
   payment, and the page or endpoint opened for you. Not reachable today: `hold` with
   `unreachable_source` and try again another day.
5. **`--rights-level allowed|restricted`: the rights level** (rights.md). `allowed`: the terms
   let anyone copy and redistribute. `restricted`: reuse is allowed under terms that must travel
   with the data. Data whose terms forbid redistribution is never cleared.

The answers are kept in the decision's journal line. Two proposals with the same content have the
same content hash (`show` prints it): decide one, `drop` the other with `duplicate`.

## Daily routine

1. Once a day: `scripts/proposal-review.sh list`. Read the first line: how full the journal is.
2. For each waiting proposal, oldest first: `show`, go through the checklist, decide.
3. **How long a proposer waits:** decide within 5 days of the proposal's age in the list. This
   is the reviewer's target; the website promises no time, and a proposer is never contacted
   (Onyx keeps no contact detail): they look at their receipt.
4. A proposal on hold is decided within 30 days: `clear` or `drop`.

**The cap.** The journal takes 10,000 proposals, decided or not; then new ones are refused
(`503 intake_full`). From 80% the node writes one line to its log with the tag
`onyx-intake-cap-alert` (at start, and once per further percent), and `list` says WARNING on its
first line. On the server: `journalctl -u onyx-node | grep onyx-intake-cap-alert`. Deciding a
proposal does not free room. At the warning, plan the archive of the journal with the node
stopped ([submission-review.md](submission-review.md)); no command for it exists yet.

A second tag, `onyx-intake-decisions-alert`, is the node's line about the decisions journal:
damaged or rule-breaking lines it ignores, decisions on receipts it does not hold, a file that
is gone or cannot be read. The node never stops for any of them and never takes a decision it
knows away while it runs. Run `list`: its WARNING lines say the same. See "Recovery".

## After a clear

Honest state today: `clear` records that a reviewer accepted the proposal for the next step, and
the receipt says so. The next step is by hand, and for most proposals it does not exist yet.

- **A source with an adapter (today only U.S. Treasury Fiscal Data):** with the node stopped,
  approve it and feed it to the observers as [submission-review.md](submission-review.md)
  describes (`submit-review … decide … approved reviewed`, then `feed-approved`). The receipt
  then shows "Approved for checking" and, after a round, the observers' value. That offline
  approval is refused unless this tool cleared the proposal first: nothing is approved with no
  checklist on record.
- **Any other source:** nothing turns a cleared proposal into a feed or a dataset. Note it for
  ROADMAP S4 (re-observation of accepted proposals) with its rights row in
  [data-sources.md](data-sources.md). The receipt stays "cleared".

## Recovery

- **A damaged line in the decisions journal** (it fails its checksum). `list` goes on and says
  how many lines it ignored; the node goes on, keeps every decision it knew and logs the alert.
  Take the next decision as usual: once that decision is allowed, and only then, the tool copies
  the damaged file to `submissions.decisions.journal.corrupt-<time>` and writes the journal
  afresh from every good line (a refused decision changes nothing). Decisions that were on a
  damaged line are gone from the file: `list --all` shows those proposals as waiting again, so
  decide them again. Keep the `.corrupt-` copy until that is done, then delete it by hand.
- **The file is not a decisions journal** (its first line, the header, is damaged, or another
  file sits under the name). Nothing in it can be told apart, so the tool never rewrites it:
  `list` says so, every decision fails with exit 1, and the running node keeps the decisions it
  knew. With the node's user, move the file away by hand and put back the last good copy from
  the node backup; decide again what was decided since that copy.
- **Rule-breaking lines** (a decision the tool would never write). They are counted, ignored by
  the tool and the node, and never shown to a proposer. Nothing to do; if you did not put them
  there, someone else writes the state directory.
- **The file is gone or unreadable while the node runs.** The node shows the last known
  decisions until it restarts. Put the file back from the node backup
  (`scripts/backup-node.sh`), owned by the node's user, mode 0600. A file over 32 MiB is not read
  at all: move it away by hand and restore the last good one.
- **Orphaned decisions** (`list` prints `orphaned sub_… <state>`): the intake journal does not
  hold their receipts, because it was archived or restored from an older copy without the
  decisions journal. They are skipped. **Always archive, restore or erase the two journals
  together**: move both away with the node stopped, or restore both from the same snapshot. A
  new intake journal gives the same receipt id to the same content at the same place, so old
  decisions left beside it are a hazard, not history.

## What is not built

- A step that makes a cleared proposal a dataset (above), and any reward for a proposal.
- Erasing a dropped proposal's text. The intake journal is append-only and the node holds it
  open, so `drop` removes nothing: the text stays in the private journal and in its backups
  ([data-retention.md](data-retention.md) "Proposals", THREATS REV-Q-4).
- Lifting a hold the intake set by itself, and an archive command for a full journal.
- A notice to the reviewer: the cap warning is a log line; the health watch does not read it
  yet and no alert channel is wired (launch checklist LC-18).
- A reviewer account or a record of who decided.

## Verification

- `cargo test -p onyx-api --lib submit::decisions` (append-only decisions across a restart,
  refusals, the node writing while the tool decides, two reviewers, the status route, the cap
  warning, the checklist, what is printed, links under the journal names, a damaged, vanished,
  unreadable or oversized decisions journal, orphaned decisions, the approval after a clear).
- `cargo test -p onyx-node --test proposals_cli` (the real command beside an open journal) and
  `cargo test -p onyx-node --lib proposals` (root drops to the journal's owner or refuses). The
  drop itself needs root: one decision is rehearsed on the server after a deploy (REV-Q-9).
- `scripts/proposal-review.sh --self-test`, `scripts/restore-drill.sh --self-test` (the backup
  reads both journals).
- Website: `node apps/web/tests/pp1.mjs` and the e2e titles starting "B2 receipt".
