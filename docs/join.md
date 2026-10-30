# Join a round as an independent observer (test network)

This page is for an operator outside Onyx who wants to check public data for the network: you run
one observer, with your own key, on your own machine. When a round draws you, your observer
fetches the public source itself, posts its own commit on chain and seals its reveal on chain. The
coordinator never holds your key and never receives a file from you.

Test networks only: Base Sepolia (chain 84532) or a local test chain. No real money is involved.

## What you need

- A Linux machine (x86-64) with `bash` and `curl`.
- The programs: an observer for the round's source (`level2-observer-<source>`) and Foundry's
  `cast`. Build them once from the Onyx source (next section), or copy them from an operator who
  did (they hold no key and no secret).
- A little test ETH on the round's chain for your commit and your reveal post (about 0.00002 ETH
  on Base Sepolia covers several rounds). Your coordinator can send it to your address.

## 1. Get the programs (once)

On Linux x86-64, download `onyx-join-linux-x86_64.tar.gz` from the latest test-network release
(the Add data page links it; check it against `SHA256SUMS`) and unpack it:
`tar xzf onyx-join-linux-x86_64.tar.gz` gives `onyx-join/`. Or build it yourself.

On a machine with Rust (the toolchain in `rust-toolchain.toml`) and Foundry installed:

```sh
git clone <the Onyx repository> onyx && cd onyx
scripts/join-build.sh ~/onyx-join
```

This builds the observers for every dated source (Treasury, USGS, NCEI) and the manifest observer
(`level2-manifest`, with the reviewed manifests in `sources/` and their terms table) in release mode
and puts them, `cast` and `join.sh` in `~/onyx-join` (`bin/` holds the programs). `join.sh observe`
picks the manifest observer when the round's metric has a manifest in the kit. Copy that folder to the
machine that will observe, if it is another one.

## 2. Make your key and send your address

```sh
~/onyx-join/join.sh key ~/onyx-join/observer.key
```

It writes a fresh key (mode 0600) and prints its address, for example
`0x3f1c…`. Send the coordinator the address only. Never send the key file.

The coordinator puts your address in the round's pool snapshot. A pool larger than the draw
means a round may not draw you; your observer then says "not drawn" and stops.

## 3. Observe a round

The coordinator sends you the round announcement, `round.json` (written by
`level2-coordinator --announce`): the chain, the registry, the fact, the record date, the
window and the pool. Start your observer before the window opens:

```sh
~/onyx-join/join.sh observe round.json ~/onyx-join/observer.key --rpc https://sepolia.base.org
```

Use your own RPC node if you have one (`--rpc`); without it the observer uses the RPC the
announcement suggests. Before it commits to anything the observer checks, by itself:

- the announcement is for the source this program observes (its own copy of the template);
- the RPC is on the chain the announcement names;
- your address is in the pool snapshot;
- the draw on chain commits to that pool, the fact's canonical spec and config, with the beacon
  fixed, and it recomputes who was drawn.

Then it waits for its fetch slot, fetches the source itself over HTTPS, commits on chain from your
account, and after commit close seals its reveal to the round's reveal key and posts it on chain.
It prints `sealed post on chain` and exits 0 when its post is in the reveal phase. Its state
(salt, commit, sealed post, mode 0600) is in `~/.onyx-join/rounds/`; a restarted observer resends
the same reveal.

Exit codes: 0 done (posted, or not drawn), 1 refused or failed (the message says why), 3 the post
landed outside the reveal phase.

## Supply data instead

A supplier proposes a dataset rather than running an observer: name a public source and a rule,
and the network's observers fetch it. Do it from an AI agent or a script with the Onyx connector
(docs/agents.md: `onyx_find_dataset`, then `onyx_propose_dataset`, then
`onyx_proposal_status`), or on the site's Add data page. A proposal costs nothing; the first
paid query of a day runs its round (NORTH-STAR NS10).

## What this does not do

- It does not make you paid: observer pay and bonds wait for the token economics (NORTH-STAR
  NS12). Today your bond in the pool snapshot is a number the coordinator declares.
- Rounds whose announcement says `"reveals": "spool"` need a file transfer to the coordinator
  (SSH/SFTP to a folder only you write); `join.sh` refuses them. Every round for outside
  observers uses sealed reveals.

Rehearsal: `scripts/join-rehearsal.sh` runs this page's steps 2 and 3 in a fresh container
(`ubuntu:26.04` with `curl` and the programs only) against a local test chain, with four
observers on the host and the container as the fifth (north-star loop NL16).
