#!/usr/bin/env python3
# The website's record of a full Onyx run on Base Sepolia (apps/web/static/run/base-sepolia-*.json),
# built only from what the run wrote: the driver's run log (.onyx-testnet/runs/run-*.log, one
# "<function>\t<basescan tx link>" line per transaction plus "spent" lines) and the console output
# of scripts/demo-testnet.sh --testnet. Nothing is typed in: every hash, address and number below is
# read from those two files, and the script stops if a step, a count or a cross-check is missing.
# It copies only function names, transaction hashes, contract addresses and the driver's step
# lines; any 64-digit hex in the output must be a transaction hash, root or shard the logs name as
# such (never a key). Runs from 2026-10-13 on also print step 6a (the API's chain indexer) and
# register several metrics with a floor; both are cross-checked and recorded when present. Runs
# from ROADMAP A2 on also settle a reserve round right after round 1 (step 5b, its transactions
# between round 1's OnyxRevenue register and the receipt path; one finality wait covers both roots)
# and sell a reserve row behind its serving check (step 8); both steps or neither, and any other
# numbered step line stops the script.
# Usage: python3 -I scripts/build-testnet-run.py <run log> <console log> <out.json>
import json, re, sys
from datetime import datetime, timezone

SCAN = "https://sepolia.basescan.org"
TX_RE = re.compile(r"^https://sepolia\.basescan\.org/tx/(0x[0-9a-f]{64})$")
H64 = re.compile(r"0x[0-9a-fA-F]{64}")
ADDR = r"0x[0-9a-f]{40}"


def fail(msg):
    sys.exit(f"build-testnet-run: {msg}")


def need(pattern, text, what):
    m = re.search(pattern, text, re.M)
    if not m:
        fail(f"{what}: no match for {pattern!r}")
    return m


def short(fn):
    return fn if fn.startswith("transfer ETH") else fn.split("(", 1)[0]


def read_run_log(path):
    lines = open(path, encoding="utf-8").read().splitlines()
    head = need(r"^# Onyx run, (\w+) mode, unix (\d+)$", lines[0], "run log header")
    addrs = need(rf"^settlement ({ADDR}) disputes ({ADDR})$", lines[1], "run log contracts")
    txs, spent = [], []
    for ln in lines[2:]:
        if ln.startswith("spent "):
            m = need(rf"^spent ({ADDR}) txs (\d+) gas (\d+) l2 (\d+) wei l1 (\d+) wei$", ln, "spent line")
            spent.append({"txs": int(m[2]), "gas": int(m[3])})
            continue
        fn, sep, url = ln.partition("\t")
        m = TX_RE.match(url)
        if not sep or not m or not re.fullmatch(r"transfer ETH|[A-Za-z]\w*\([\w,()\[\]]*\)", fn):
            fail(f"unexpected run log line: {ln[:60]!r}")
        txs.append({"fn": short(fn), "hash": m[1]})
    return {"mode": head[1].lower(), "unix": int(head[2]), "settlement": addrs[1], "disputes": addrs[2]}, txs, spent


# Run-log transactions in order, as phases of the driver: each phase takes the longest run of
# transactions whose function is in its set (at least one). Phases of the "r3" group (the reserve
# round, step 5b) are there exactly when the console has steps 5b and 8.
PHASES = [
    ("fund", {"transfer ETH", "transfer", "setReporter"}),
    ("setup", {"mint", "approve", "postCoordinatorBond", "fundRewards", "stake"}),
    ("registry", {"registerMetric", "registerLabelSet", "attestShard"}),
    ("r1.open", {"openRound"}),
    ("r1.commit_reveal", {"commit", "reveal"}),
    ("r1.verdicts", {"openCanaries", "drawSeed", "commitVerdict", "revealVerdict"}),
    ("r1.finalize", {"finalizeShard", "release"}),
    ("r1.withdraw", {"withdraw"}),
    ("r1.revenue", {"register"}),
    ("r3.setup", {"fundRewards"}),
    ("r3.registry", {"registerMetric", "registerLabelSet", "attestShard"}),
    ("r3.open", {"openRound"}),
    ("r3.commit_reveal", {"commit", "reveal"}),
    ("r3.verdicts", {"openCanaries", "drawSeed", "commitVerdict", "revealVerdict"}),
    ("r3.finalize", {"finalizeShard", "release"}),
    ("r3.revenue", {"register"}),
    ("r1.receipt", {"claim", "withdraw"}),
    ("r2.attest", {"attestShard"}),
    ("r2.open", {"openRound"}),
    ("r2.commit_reveal", {"commit", "reveal"}),
    ("r2.verdicts", {"openCanaries", "drawSeed", "commitVerdict", "revealVerdict"}),
    ("r2.finalize", {"finalizeShard"}),
    ("r2.case", {"openRerun", "draw", "commitVote", "revealVote", "resolve"}),
    ("r2.settle", {"settleCase"}),
    ("sweep", {"transfer ETH"}),
]


def phases(txs, reserve):
    out, i = {}, 0
    for name, fns in PHASES:
        if name.startswith("r3.") and not reserve:
            continue
        j = i
        while j < len(txs) and txs[j]["fn"] in fns:
            j += 1
        if j == i:
            fail(f"phase {name}: no transaction at position {i}")
        out[name] = txs[i:j]
        i = j
    if i != len(txs):
        fail(f"{len(txs) - i} run log transactions after the last phase")
    return out


def read_console(path):
    text = open(path, encoding="utf-8").read()
    # Retries and the actor sweep are dropped; everything else is a step line or the summary.
    noise = re.compile(r"^  \S.*: estimate refused \(\d+ of \d+\); asking again$|^  node unreachable \(\d+ of \d+\); retrying$")
    steps, pre, cur = {}, [], None
    for ln in text.splitlines():
        if noise.match(ln):
            continue
        m = re.match(r"^(\d+[ab]?)\. (.*)$", ln)
        if m:
            cur = steps[m[1]] = {"pre": pre, "lines": [m[2]]}
            pre = []
        elif cur and re.match(r"^   \S", ln):
            cur["lines"].append(ln.strip())
        elif re.match(r"^  (registry|preflight): ", ln):
            pre.append(ln.strip())
            cur = None
        elif ln == "Summary":
            break
        elif re.match(r"^\d+\w*\. ", ln):
            fail(f"console: unknown step line {ln[:20]!r}")
        else:
            cur = None
    for k in ("0", "1", "2", "3", "4", "5", "6", "6b", "7"):
        if k not in steps:
            fail(f"console: step {k} missing")
    # Step 6a (the API's chain indexer, ADR-0033) is printed by runs from 2026-10-13 on.
    for k in steps:
        if k not in ("0", "1", "2", "3", "4", "5", "5b", "6a", "6", "6b", "7", "8"):
            fail(f"console: unknown step {k}")
    # The reserve round (ROADMAP A2): settled in 5b, sold in 8; one without the other is refused.
    if ("5b" in steps) != ("8" in steps):
        fail("console: steps 5b and 8 (the reserve round) come together")
    return text, steps


def main():
    if len(sys.argv) != 4:
        fail("usage: build-testnet-run.py <run log> <console log> <out.json>")
    run_path, con_path, out_path = sys.argv[1:]
    meta, txs, spent = read_run_log(run_path)
    con, st = read_console(con_path)
    has_reserve = "5b" in st
    ph = phases(txs, has_reserve)

    # The verdict and the network.
    if not re.search(r"^REHEARSAL PASSED$", con, re.M):
        fail("console: no REHEARSAL PASSED line")
    net = need(r"^Onyx M8 run, (\w+) mode, chain (\d+) \(([^)]+)\)$", con, "console network")
    if net[1].lower() != meta["mode"] or net[1].lower() != "testnet" or net[2] != "84532":
        fail("console and run log disagree on the mode or chain")
    c1 = need(rf"^  settlement ({ADDR})  disputes ({ADDR})$", con, "console contracts")
    if (c1[1], c1[2]) != (meta["settlement"], meta["disputes"]):
        fail("console and run log disagree on the contracts")
    c2 = need(rf"^  revenue    ({ADDR}) \(x402 payTo\)  usdc ({ADDR})$", con, "console revenue")
    dep = need(r"^  deploy: (\d+) txs, (\d+) L2 gas", con, "deploy line")

    # The summary block.
    S = lambda p, w: need(p, con, f"summary {w}")
    s_data = S(r"^  data       (live blocks (\d+)-(\d+)) \((\d+) rows\)$", "data")
    s_shard = S(r"^  round      (\d+), shard (0x[0-9a-f]{64})$", "round")
    s_root = S(r"^  root       (0x[0-9a-f]{64})$", "root")
    s_open = S(r"^  open tx    (0x[0-9a-f]{64})$", "open tx")
    s_rel = S(r"^  release tx (0x[0-9a-f]{64})$", "release tx")
    s_okx = S(r"^  OKX        in ([\d.]+) BTC, out ([\d.]+) BTC, net (-?[\d.]+) BTC \((\d+) txs\)$", "OKX")
    s_chain = S(r"^  chain      (\d+) transactions, \d+ blocks, (\d+) s$", "chain")
    s_spent = S(r"^  spent      (\d+) txs, (\d+) L2 gas, ([\d.]+) ETH on L2 \+ ([\d.]+) ETH L1 data fees$", "spent")
    sweep = S(r"^  sweep: (\d+) actor keys returned ([\d.]+) ETH to the deployer$", "sweep")

    # Cross-checks between the two logs.
    chain_txs, spent_txs, n_sweep = int(s_chain[1]), int(s_spent[1]), int(sweep[1])
    if len(ph["sweep"]) != n_sweep:
        fail("sweep count differs between the logs")
    if len(txs) - n_sweep != chain_txs:
        fail(f"run log has {len(txs) - n_sweep} run transactions, the summary says {chain_txs}")
    if chain_txs + int(dep[1]) != spent_txs or sum(s["txs"] for s in spent) != spent_txs + n_sweep:
        fail("transaction counts do not add up (run + deploy = spent; spent lines = spent + sweep)")
    if ph["r1.open"][0]["hash"] != s_open[1] or ph["r1.finalize"][-1]["hash"] != s_rel[1]:
        fail("open/release tx differ between the logs")
    setup = need(r"\((\d+) txs\)$", st["1"]["lines"][0], "setup count")
    if int(setup[1]) != len(ph["fund"]) + len(ph["setup"]):
        fail("setup transaction count differs between the logs")

    s3, s4, s5 = (" ".join(st[k]["lines"]) for k in ("3", "4", "5"))
    s6, s6b, s7 = (" ".join(st[k]["lines"]) for k in ("6", "6b", "7"))
    m3 = need(r"^(\d+) miners ran the recipe \(([\d.]+) s each\), committed ([\d.]+) USDC bonds and revealed root (0x[0-9a-f]{8})", s3, "step 3")
    if not s_root[1].startswith(m3[4]):
        fail("step 3 root differs from the summary root")
    m4 = need(r"(\d+) validators reran the recipe \(([\d.]+) s\): (\d+)/(\d+) Agree; (\d+) stand by", s4, "step 4")
    need(r"\(contract and Rust equal\)", s4, "step 4 seed")
    m5 = need(r"^finalized at block (\d+); no challenge in (\d+) blocks; withdrawn and checked: first committer ([\d.]+) USDC \(bond \+ reward\), the other ([\d.]+) USDC \(bond\); (\d+) checkers ([\d.]+) USDC each", s5, "step 5")
    # One finality wait: after round 1 (step 5), or after the reserve round settled right behind it
    # (step 5b), for both roots.
    s5b = " ".join(st["5b"]["lines"]) if has_reserve else ""
    fin = need(
        r"the root reached Base Sepolia's finalized block after (\d+) s" if not has_reserve
        else r"the roots of rounds 1 and 3 reached Base Sepolia's finalized block after (\d+) s \(one wait for both\)",
        s5b or s5, "finality")
    m6 = need(r"unpaid -> (\d+); paid (\d+) micro-USDC with x402 .*?settled: true\) -> (\d+)", s6, "step 6")
    need(r"checked the answer against its own getFinalizedRoot call; the contract's verifyRow says true", s6, "step 6 verify")
    lev = need(r"verifyRowLeveled says true at proof level (\d+)", s6, "step 6 leveled")
    settle = need(r"settle tx (0x[0-9a-f]{64})", s6, "settle tx")
    rcpt = need(r"transfer (0x[0-9a-f]{64}) moved (\d+) micro-USDC", s6b, "receipt transfer")
    cred = need(r"credit tx (0x[0-9a-f]{64}) applied (\d+) of (\d+)", s6b, "credit tx")
    wd = need(r"withdrew (\d+) micro-USDC .*? in tx (0x[0-9a-f]{64})", s6b, "miner withdraw")
    if ph["r1.receipt"][-1]["hash"] != wd[2]:
        fail("miner withdraw tx differs between the logs")
    need(r"root equals the pinned v1 live root; OKX equals the Python recount \(checked\)", s6b, "live data check")
    m7 = need(r"^dispute run \(round (\d+), fixture blocks (\d+)-(\d+), zero starting balances\): miner B revealed a made-up root; (\d+) validator\(s\) disagreed", s7, "step 7")
    pan = need(r"a panel of (\d+) standby validators was drawn; each reran the recipe and voted for the true root", s7, "panel")
    dec = need(r"the true root (0x[0-9a-f]{8})\.\.\. is final; checked: miner A credited \+([\d.]+) USDC \(bond \+ reward\), miner B \+([\d.]+) \(its ([\d.]+) USDC bond was taken\)", s7, "decision")
    need(r"level-1 attestation is refused \(BelowMetricLevel\), level (\d+) passes", " ".join(st["2"]["pre"]), "registry floor")
    need(r"tag computed by the contract and by Rust: equal", st["2"]["lines"][0], "step 2 tag")
    # Metrics registered with their recipe and a proof-level floor (the reserve metric from 2026-10-13 on).
    reg = [
        {"metric": m[1], "floor": int(m[2])}
        for ln in st["2"]["pre"]
        for m in [re.match(r"^registry: ([a-z][a-z0-9_.]*) registered with its recipe and floor (\d+) in one call; on chain a level-1 attestation is refused \(BelowMetricLevel\), level (\d+) passes$", ln)]
        if m
    ]
    if not reg:
        fail("console: no metric registered with a floor")
    if any(int(m[1]) != int(lev[1]) for ln in st["2"]["pre"] for m in [re.search(r"level (\d+) passes$", ln)] if m):
        fail("registry floor level differs from the attested proof level")
    # The API's chain indexer (step 6a): the round-1 shard found from Finalized events.
    idx = None
    if "6a" in st:
        s6a = " ".join(st["6a"]["lines"])
        ix = need(r"^indexer: read blocks (\d+)\.\.(\d+) \(deploy to finalized\) in (\d+) eth_getLogs calls \((\d+) split\); Finalized\(round (\d+), shard (0x[0-9a-f]{8})\.\.\.\) at block (\d+): the contract's getFinalizedRoot and the held rows agree, so the API serves it \(level (\d+)\)$", s6a, "step 6a")
        if int(ix[5]) != int(s_shard[1]) or not s_shard[2].startswith(ix[6]):
            fail("step 6a round or shard differs from the summary")
        # Step 5 prints the head the driver read after finalizeShard returned (eth_blockNumber), the
        # indexer the block of the Finalized log itself: on a live chain the head can be a block or
        # two ahead (2026-10-14: 48113186 vs 48113187), never behind.
        if not 0 <= int(m5[1]) - int(ix[7]) <= 5:
            fail("step 6a finalized block is not at most 5 blocks before step 5's head")
        if int(ix[8]) != int(lev[1]):
            fail("step 6a level differs from the attested proof level")
        if not int(ix[1]) <= int(ix[7]) <= int(ix[2]):
            fail("step 6a finalized block outside the blocks read")
        idx = {
            "from_block": int(ix[1]),
            "to_block": int(ix[2]),
            "get_logs_calls": int(ix[3]),
            "split_calls": int(ix[4]),
            "round": int(ix[5]),
            "finalized_block": int(ix[7]),
            "level": int(ix[8]),
        }

    # The reserve round (steps 5b and 8): settled on chain, indexed, cleared by its serving check,
    # sold with x402 into OnyxRevenue, Binance refused, a forged base refused.
    res = None
    if has_reserve:
        s8 = " ".join(st["8"]["lines"])
        r5 = need(r"^reserve round (\d+) \(reserve tail blocks (\d+)-(\d+), label set v2\): btc-entity-reserve on the state after block (\d+) \(base_ref = the committed scan's hash\); (\d+) miners ran it \(([\d.]+) s each\), (\d+)/(\d+) verdicts Agree; finalized at block (\d+), root (0x[0-9a-f]{8})\.\.\.; released after the (\d+)-block challenge window", s5b, "step 5b")
        need(r"OnyxRevenue: the reserve shard is registered", s5b, "step 5b revenue")
        r_sum = S(r"^  reserve    round (\d+), shard (0x[0-9a-f]{64})$", "reserve round")
        r_root = S(r"^  res\. root  (0x[0-9a-f]{64})$", "reserve root")
        if int(r_sum[1]) != int(r5[1]) or not r_root[1].startswith(r5[10]):
            fail("step 5b round or root differs from the summary")
        r8 = need(r"^reserve shard \(round (\d+)\) indexed: Finalized\(round (\d+)\) found, held rows hash to the chain's root, admitted at level (\d+); serving check ([a-z0-9-]+): the chain from block (\d+) lands on the committed scan at (\d+) .*?shard cleared", s8, "step 8 index")
        if int(r8[1]) != int(r5[1]) or int(r8[2]) != int(r5[1]) or int(r8[5]) != int(r5[4]) or int(r8[6]) != int(r5[3]):
            fail("step 8 round or heights differ from step 5b")
        sale = need(r"OKX unpaid -> (\d+); paid (\d+) micro-USDC with x402 \(self-settled into OnyxRevenue, settled: true\) -> (\d+), ([\d.]+) BTC at block (\d+) \(= the scan's OKX total\), crossCheck ([a-z-]+); the client checked it against its own getFinalizedRoot, verifyRow true, verifyRowLeveled true at level (\d+)", s8, "step 8 sale")
        r_settle = need(r"reserve settle tx (0x[0-9a-f]{64}): OnyxRevenue holds the USDC and booked (\d+) micro-USDC to the reserve shard in the same transaction", s8, "step 8 settle")
        if r_settle[2] != sale[2] or int(sale[7]) != int(r8[3]):
            fail("step 8 booked amount or level differs")
        need(r"Binance -> 403 not_cross_checked before any 402", s8, "step 8 Binance")
        forged = need(r"forged base \(OKX \+([\d.]+) BTC in the spec's params\): both miners' reruns agree on root (0x[0-9a-f]{8})\.\.\., and the serving check refuses it: ", s8, "step 8 forged")
        res = {
            "round": int(r5[1]),
            "shard": r_sum[2],
            "root": r_root[1],
            "blocks": f"{r5[2]}-{r5[3]}",
            "base_height": int(r5[4]),
            "verdicts_agree": int(r5[7]),
            "verdicts": int(r5[8]),
            "finalized_block": int(r5[9]),
            "gate": r8[4],
            "level": int(r8[3]),
            "unpaid_status": int(sale[1]),
            "paid_status": int(sale[3]),
            "price_micro_usdc": sale[2],
            "okx_btc": sale[4],
            "okx_block": int(sale[5]),
            "okx_cross_check": sale[6],
            "settle_tx": r_settle[1],
            "binance_status": 403,
            "forged_extra_btc": forged[1],
        }

    def T(*names, extra=()):
        out = [dict(t, src="run log") for n in names for t in ph[n]]
        return out + [dict(fn=f, hash=h, src="console") for f, h in extra]

    def step(sid, title, check, lines, txs_):
        return {"id": sid, "title": title, "check": check, "log": lines, "txs": txs_}

    steps = [
        step("fund", "Gas and test USDC handed out", "The API operator is OnyxRevenue's reporter; the customer holds test USDC", st["0"]["lines"], T("fund")),
        step("setup", "Stakes and bonds posted", f"With step 1: {len(ph['fund']) + len(ph['setup'])} transactions, the driver's own count", st["1"]["lines"], T("setup")),
        step("registry", "Recipe registered, proof level attested" if len(reg) == 1 else "Metrics registered, proof level attested",
             (f"{len(reg)} metrics with floor {'/'.join(sorted({str(r['floor']) for r in reg}))} ({', '.join(r['metric'] for r in reg)}); a" if len(reg) > 1 else "A")
             + f" level-1 attestation is refused on chain; level {lev[1]} passes", st["2"]["pre"], T("registry")),
        step("r1.open", "Round 1 opens on live Bitcoin blocks", "Round tag: contract and Rust equal", st["2"]["lines"], T("r1.open")),
        step("r1.commit_reveal", "Miners commit, then reveal", f"{m3[1]} miners, {m3[3]} USDC bond each, same root", st["3"]["lines"], T("r1.commit_reveal")),
        step("r1.verdicts", "Validators rerun the recipe", f"{m4[3]}/{m4[4]} agree; seed: contract and Rust equal", st["4"]["lines"], T("r1.verdicts")),
        step("r1.finalize", "Root final, everyone paid", f"First committer {m5[3]} USDC, other {m5[4]}, {m5[5]} checkers {m5[6]} each; on Base's finalized block after {fin[1]} s" + (" (one wait with the reserve round)" if res else ""), st["5"]["lines"], T("r1.finalize", "r1.withdraw", "r1.revenue")),
        *([step("r3.settle", "A reserve round settles on chain", f"btc-entity-reserve over blocks {res['blocks']}; {res['verdicts_agree']}/{res['verdicts']} agree; finalized at block {res['finalized_block']}; registered in OnyxRevenue", st["5b"]["pre"] + st["5b"]["lines"], T("r3.setup", "r3.registry", "r3.open", "r3.commit_reveal", "r3.verdicts", "r3.finalize", "r3.revenue"))] if res else []),
        *([step("r1.index", "The API finds the root on chain", f"Blocks {idx['from_block']}..{idx['to_block']} in {idx['get_logs_calls']} eth_getLogs calls; Finalized at block {idx['finalized_block']}; getFinalizedRoot and the held rows agree", st["6a"]["lines"], [])] if idx else []),
        step("r1.serve", "Served, paid with x402, verified", f"{m6[1]} unpaid, {m6[3]} paid; client matched getFinalizedRoot; verifyRow true; verifyRowLeveled true at level {lev[1]}", st["6"]["lines"], T(extra=[("settle", settle[1])])),
        step("r1.receipt", "A crash keeps the receipt", f"Credit applied {cred[2]} of {cred[3]}; miner withdrew {wd[1]} micro-USDC", st["6b"]["lines"], T("r1.receipt", extra=[("transfer", rcpt[1]), ("credit", cred[1])])),
        step("r2.dispute", "A cheater is caught", f"{m7[4]} validators disagree; a panel of {pan[1]} votes the true root; the cheater's {dec[4]} USDC bond is taken", st["7"]["pre"] + st["7"]["lines"], T("r2.attest", "r2.open", "r2.commit_reveal", "r2.verdicts", "r2.finalize", "r2.case", "r2.settle")),
        *([step("r3.serve", "A reserve row sold behind its serving check", f"Indexed at level {res['level']}; {res['gate']} cleared it; OKX {res['unpaid_status']} unpaid, {res['paid_status']} paid ({res['price_micro_usdc']} micro-USDC booked to the shard), {res['okx_btc']} BTC = the scan; Binance 403; a forged base refused", st["8"]["lines"], T(extra=[("settle", res["settle_tx"])]))] if res else []),
        step("sweep", "Leftover gas returned", f"{n_sweep} transfers back to the deployer ({sweep[2]} ETH)", [], T("sweep")),
    ]
    # The receipt step's transactions in the order they happened: transfer, credit, then claim and withdraw.
    rc = next(s for s in steps if s["id"] == "r1.receipt")
    rc["txs"] = rc["txs"][2:] + rc["txs"][:2]

    contracts = [
        {"name": "OnyxSettlement", "address": meta["settlement"]},
        {"name": "OnyxDisputes", "address": meta["disputes"]},
        {"name": "OnyxRevenue (x402 payTo)", "address": c2[1]},
        {"name": "USDC (Base Sepolia test token)", "address": c2[2]},
    ]
    for c in contracts:
        c["url"] = f"{SCAN}/address/{c['address']}"
    for s in steps:
        for t in s["txs"]:
            t["url"] = f"{SCAN}/tx/{t['hash']}"

    started = re.search(r"run-(\d{8}T\d{6}Z)\.log$", run_path)
    out = {
        "generated_by": "scripts/build-testnet-run.py <run log> <console log>",
        "sources": {"run_log": run_path.rsplit("/", 1)[-1], "console": "scripts/demo-testnet.sh --testnet output"},
        "verdict": "REHEARSAL PASSED",
        "network": net[3],
        "chain_id": int(net[2]),
        "mode": meta["mode"],
        "date": datetime.strptime(started[1], "%Y%m%dT%H%M%SZ").strftime("%Y-%m-%d") if started else datetime.fromtimestamp(meta["unix"], timezone.utc).strftime("%Y-%m-%d"),
        "started": datetime.strptime(started[1], "%Y%m%dT%H%M%SZ").strftime("%Y-%m-%dT%H:%M:%SZ") if started else None,
        "explorer": SCAN,
        "contracts": contracts,
        "deploy": {"txs": int(dep[1]), "l2_gas": int(dep[2])},
        "data": s_data[1],
        "rows": int(s_data[4]),
        "round": {
            "round": int(s_shard[1]),
            "shard": s_shard[2],
            "root": s_root[1],
            "open_tx": s_open[1],
            "release_tx": s_rel[1],
            "miners": int(m3[1]),
            "validators": int(m4[1]),
            "agrees": int(m4[3]),
            "verdicts": int(m4[4]),
            "standbys": int(m4[5]),
            "finalized_block": int(m5[1]),
            "challenge_window_blocks": int(m5[2]),
            "seconds_to_base_finality": int(fin[1]),
            "price_micro_usdc": m6[2],
            "unpaid_status": int(m6[1]),
            "paid_status": int(m6[3]),
            "proof_level": int(lev[1]),
            "okx_net_btc": s_okx[3],
        },
        "registry": reg,
        "indexer": idx,
        "dispute": {
            "round": int(m7[1]),
            "data": f"fixture blocks {m7[2]}-{m7[3]}",
            "validators_disagreed": int(m7[4]),
            "panel_size": int(pan[1]),
            "honest_credit_usdc": dec[2],
            "cheater_credit_usdc": dec[3],
            "cheater_bond_taken_usdc": dec[4],
        },
        "totals": {
            "run_txs": chain_txs,
            "sent_txs": spent_txs,
            "l2_gas": int(s_spent[2]),
            "l2_eth": s_spent[3],
            "l1_eth": s_spent[4],
            "secs": int(s_chain[2]),
            "sweep_txs": n_sweep,
            "linked_txs": sum(len(s["txs"]) for s in steps),
        },
        "steps": steps,
    }
    if res:
        out["reserve"] = res

    # Every 64-digit hex string shipped must be one the logs name as a transaction, root or shard.
    named = {t["hash"] for t in txs} | {settle[1], rcpt[1], cred[1], s_root[1], s_shard[2], s_open[1], s_rel[1]}
    if res:
        named |= {res["settle_tx"], res["shard"], res["root"]}
    body = json.dumps(out, indent=2) + "\n"
    stray = set(H64.findall(body)) - named
    if stray or re.search(r"(?<![0-9a-fA-Fx])[0-9a-fA-F]{64}(?![0-9a-fA-F])", body):
        fail("output carries a 64-digit hex string the logs do not name as a tx, root or shard")
    if re.search(r"(?i)private|secret|mnemonic|\bkeys?\b", body):
        fail("output mentions keys")
    open(out_path, "w", encoding="utf-8").write(body)
    print(f"{out_path}: {len(steps)} steps, {out['totals']['linked_txs']} tx links, {len(contracts)} contracts")


main()
