# Usage counts (READINESS A9)

## Scope

The node keeps no access log, and the proxy in front of it keeps none either. So nobody could say
which routes and datasets are used. This spec adds counters inside `onyx-node` that answer that
question and keep nothing about who asked. It covers what is counted, what is stored, how long, and
who can read it. Decision: ADR-0008 amendment 1. Threats: THREATS.md `USE-1` to `USE-7` and the
review `USE-R1`.

Off by default. `ONYX_USAGE=on` turns it on (docs/ops/config.md); off means nothing is counted and
nothing is written.

## What is counted

One answered request adds 1 to at most two numbers of the current UTC day:

1. **Its route and status class.** The key is the method and the route's template as the router
   knows it, for example `GET /v1/bulk/{publisher}/{dataset}/rows`: the template, never the path
   that was asked. The method is one of a fixed list of words (an unusual one is `OTHER`). A request
   that matches no route counts under `GET (no route)`. Every 405 answer counts under the one key
   `(method not allowed)`. The status class is `2xx`, `3xx`, `4xx` or `5xx`.
2. **Its dataset, when the request opens one.** An open is an answered (2xx) request for a dataset's
   info (`/v1/bulk/{publisher}/{dataset}`) or for the first page of its rows (`.../rows` with page
   0), for a dataset the node holds. The key is `publisher/dataset` as the node's own store spells
   it. Later pages of rows and the successor link are requests on their route, not opens: a reader
   who pages through a dataset is counted once or twice, not once per page.

Every caller is counted the same way: a visitor through the proxy, the hosted AI connector, the
health watch, a storage peer, the operator. The counts do not say which was which.

A request whose client goes away before the answer is not counted. A connection the server closes
before reading a request (a connection cap) is not a request.

## What is stored: one file per day

`<ONYX_STATE_DIR>/usage/<YYYY-MM-DD>.json`, one line of JSON, readable by the node's user only (the
folder 0700, each file 0600). This is the whole schema:

```json
{
  "version": 1,
  "day": "2026-11-16",
  "routes": {
    "GET /v1/health": { "2xx": 1440, "3xx": 0, "4xx": 0, "5xx": 0 },
    "GET /v1/bulk/{publisher}/{dataset}/rows": { "2xx": 310, "3xx": 0, "4xx": 12, "5xx": 0 }
  },
  "datasets": { "example.publisher/synthetic.metrics": 42 },
  "datasets_other": 17,
  "small_folded": false
}
```

| Field | Meaning |
|---|---|
| `version` | 1 |
| `day` | The UTC day the counts belong to (the file's name) |
| `routes` | Method and route template, then requests by status class |
| `datasets` | `publisher/dataset`, then opens (above) |
| `datasets_other` | Opens not counted by name: past the daily cap of names, or folded for a small count |
| `small_folded` | `true` only for a day that is over, whose small counts were folded and which nothing was added to since (below) |

Nothing else is written, anywhere. In particular there is no address, no port, no user agent, no
referrer, no cookie, no header of any kind, no query text, no path value (only the template), no
payment or account id, no request id, no time within the day, no duration, no size, and no order of
requests. The code reads none of these to count, except one number of the rows query: whether the
page is the first. The file is read back strictly: a field the code does not know makes the file
invalid, so a field cannot be added to the file without changing the struct
`onyx_api::usage::DayFile` and the test that pins its keys.

### What a count can and cannot say

The counts are numbers of requests. They are not numbers of people, and nothing here makes them
so: telling two requests of one person from two people needs something kept about the person, which
is exactly what is not kept.

- No two requests are linked: there is no id, no sequence and no time finer than a day.
- Every key is a string the server chose. Route templates come from the router. Dataset names come
  from the store of datasets the node holds. A string a client invents (a path, a dataset that does
  not exist, a query) never becomes a key, so a client cannot write a name, a token or a message
  into the counts.
- What a reader of the counts can still learn: that a dataset was opened on a day, and roughly how
  often. If the reader knows from elsewhere that only one party uses a dataset, a count tells how
  active that party was that day, to the day. The next section limits this for rarely opened
  datasets; it does not remove it. One reader who opens a dataset five times in a day is named
  like five readers who open it once.

### Small counts

A dataset opened once on one day is the easiest to match with one person. So a dataset with **fewer
than 5 opens** on a day is never named:

- The operator's report takes such datasets out of the names, for every day, also for today. The
  rule is per day: a dataset with 4 opens on each of 7 days is never named.
- Their opens are summed into `datasets_other` when **three or more** datasets are taken out on that
  day. With one or two, "other" would be those datasets' own count under another name (the held
  datasets are public and the named ones are in the report), so their opens are left out of
  `datasets_other` as well. They remain in the route counts, which name no dataset.
- A finished day's file is written one last time with this done for good (`small_folded: true`).
  From then on the exact small numbers exist nowhere. This also happens for a day the node did not
  finish itself (it was down when the day ended): once a day the node folds every past day file that
  is not folded yet.
- Today's file holds the exact numbers until the day ends. It must: the counts have to survive a
  restart, and a count cannot be folded before the day's total is known. Today's file is the one
  place exact small numbers are kept, for at most a day, readable by the node's user only.
- Backups keep folded files only. `scripts/backup-node.sh` does not copy today's or yesterday's
  file, nor a set-aside `.corrupt` one, and deletes from its copy any day file without
  `"small_folded": true`. A backup is kept as long as the backup's own schedule says (weeks and
  months), which is longer than the node's 90 days; what it holds of the counts is finished, folded
  days.
- A clock that steps back into a finished day adds to a folded file. That day's file is then written
  as not folded while the day is live and folded again when it is over.

Route counts are not folded. A route template says what kind of request was made, never about
which subject.

### Bounds

| Bound | Value |
|---|---|
| Dataset names per day | 2,000; a further dataset that day adds to `datasets_other` |
| Route keys per day | 256, then the key `(other)` (the node has a few dozen templates; 405 answers share one key) |
| Days kept | 90 (today and the 89 before); older files are deleted |
| Days in memory | Today, plus at most 2 waiting for a write that failed |
| One day's file | Under 1 MiB at the caps; a larger file is not read |
| Disk at the caps | About 55 MiB for 90 days |

The first 2,000 held datasets opened on a day get a name; which ones is a matter of arrival order.
A day with more than 2,000 distinct datasets opened undercounts the later ones by name; their opens
are still in `datasets_other` and their requests in the route counts.

## Interfaces

- **Counting**: `onyx_api::usage::counted`, the outermost layer of the node's router, so refusals by
  the layers inside it (rate limit, overload, deadline) are counted too. One request costs a bounded
  number of map steps under one short lock; no lock is held across an `await`; no string of a size
  the client chooses is allocated (a dataset id is at most 128 bytes and is checked before the store
  is asked).
- **Writing**: every 60 seconds, and once more at shutdown after the last request was answered. A
  write is a temporary file, an fsync and a rename. Only days with new counts are written.
- **Start**: nothing is read at start. At the first write of a day the day's file, if there is one,
  is added to the counts in memory: a restart loses at most the last minute.
- **The daily sweep** (at the first write of a UTC day): files older than 90 days are deleted, and a
  past day's file that is not folded is folded.
- **A corrupt file** (not JSON, a wrong version or day, an unknown field, a bad key, too large) is
  renamed to `<day>.json.corrupt`, one log line says so, and counting goes on with a clean file. The
  node never stops for it.
- **A failed write** (a full disk) is one log line a minute; the counts stay in memory for the next
  write. Days do not pile up: past 3 in memory the oldest is dropped.
- **Reading: no route.** The node has no HTTP route that gives the counts out, on any listener.
  `/v1/usage` is the ordinary 404. The operator reads the files, on the host:

  ```sh
  sudo onyx-node usage --days 7 --state-dir /var/lib/onyx/state
  ```

  `onyx-node usage [--days N] [--until YYYY-MM-DD] [--state-dir DIR]` (N 1 to 90, default 7; the
  state directory from `ONYX_STATE_DIR` when not given) prints one JSON report:

  ```json
  {
    "version": 1, "from": "2026-11-10", "to": "2026-11-16", "small_count": 5,
    "note": "...", "unreadable_days": [],
    "totals": { "routes": {}, "datasets": {}, "datasets_other": 0 },
    "days": [ { "day": "2026-11-16", "routes": {}, "datasets": {}, "datasets_other": 0 } ]
  }
  ```

  It reads the day files and nothing else. It does not ask the running node, so counts the node has
  not written yet (at most its last minute) are missing; it writes and renames nothing; each file is
  read once and files are replaced whole, so a write by the node during the read cannot double a
  count. It applies the small-count rule to every day. Exit 2 on a bad argument, 1 when the folder
  cannot be read. `scripts/usage-report.sh [DAYS] [TOP]` runs it on the server over ssh and prints
  the top routes and datasets.
- **Backups**: see "Small counts": folded day files only.

### Why there is no read route

A first build had a route that answered only a request from the host itself (a loopback peer, no
forwarding header, an operator header). Review `USE-R1` (M2) found that this rests on two things the
node cannot check: that the proxy adds its forwarding header, and that nothing else on the host
forwards connections. Any TCP forwarder on the host (an ssh remote forward, a relay, a hidden
service) makes a remote peer look local with no such header, and every local account could read
counts whose files are private. The header was public, so it stopped nothing.

Reading the files has neither problem: who may read is the file system's question (the state
directory belongs to the service; on the hosted server only root can enter it), nothing listens,
and nothing a remote peer sends can reach the reader.

## Assumptions

- The state directory is private to the node's user (the shipped systemd unit: `StateDirectoryMode`
  0700 under a dynamic user). Whoever can read it as root, or holds a backup, can read the day
  files; a backup holds folded days only.
- The host clock gives the UTC day. A clock that jumps moves counts to the day it then shows; no
  count is lost or doubled, and a day's file that already exists is added to.
- The counts are meant for the operator; they are not published.
- The counts are for capacity and for knowing what is used. They are never a measure of popularity
  for rewards, payments or rankings: anyone can add requests (THREATS `USE-4`).
- Local services (the AI connector, the health watch) are counted with everyone else, so the totals
  are above what visitors alone asked.

## Open questions

- A public, coarser summary (for example the 20 most opened datasets of a month, rounded) would help
  publishers. Not built: it needs its own threshold and its own review, and counts that pay nobody
  must stay unpaid.
- Storage nodes can run with `ONYX_USAGE=on` too, each counting its own requests. Nothing adds
  nodes together yet.
- The small-count line is 5 opens a day, a number chosen, not derived. A dataset that only one
  party uses is still named above it; a higher line or a weekly report only would hide more and
  tell the operator less.
