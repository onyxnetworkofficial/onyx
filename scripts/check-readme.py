#!/usr/bin/env python3
"""Keep the repository's maps current (pre-public loop PP7). Run by scripts/check-docs.sh.

Generated from the repository itself, never typed twice:
- README.md, block `layout`: every crate, adapter, app, recipe, experiment and top-level folder,
  each described by its own `Cargo.toml` `description` or, for a folder without one, by the
  table below; and every contract in contracts/src by its `@title`/`@notice`.
- README.md, block `routes`: every operation of docs/api/openapi.json.
- scripts/README.md: every script, grouped, described by its own header comment.
- docs/README.md, block `docs`: every folder and file under docs/, by the table below.

`--write` regenerates them; without it the check fails when a block is stale or a new folder,
script or document has no description. `--self-test` plants faults and requires each to fail.
"""
import json, os, re, sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# Folders that have no Cargo.toml description of their own.
FOLDERS = {
    "apps/agent": "The agent toolkit: an API client and an MCP server built from the OpenAPI file; answers are checked by onyx-check",
    "apps/contrib": "The contributor client (onyx-contrib): keep API keys in the system keychain and contribute their data over MCP; keys never leave the machine",
    "apps/demo": "The first demo site: one page that walks the pipeline and reruns the real recipe in the browser",
    "apps/web": "The website (SvelteKit): every page runs the network's own Rust checks, compiled to WebAssembly",
    "recipes/out": "The built recipe WASM files and their Keccak-256 ids (reproducible, committed)",
    "contracts": "The Foundry project: settlement, disputes, revenue, registry, level-3 book, stakes and futures (Base Sepolia only)",
    "deploy": "Caddy TLS front, systemd units, health-watch thresholds, the reserve serving gate",
    "docs": "Specs, decision records, operations, audits, partner notes, the API description",
    "fixtures": "Pinned inputs, goldens and shared test vectors (Rust, Solidity and the browser read the same files)",
    "fuzz": "cargo-fuzz targets over decoders, stores and proofs (opt-in, nightly)",
    "scripts": "Checks, builds, rehearsals, recounts and data refreshes (scripts/README.md)",
    "sources": "Reviewed source manifests: a public JSON API reaches level 2 as data, with its terms and one recording (ADR-0053)",
    "experiments/tlsn-fiscaldata": "A real TLSNotary proof of a U.S. Treasury API answer with a hidden header, verified offline (its own workspace)",
}
SKIP_TOP = {"target", "node_modules", ".git", ".github", ".claude"}
# A public snapshot folder is not a git checkout: every folder in it is the tree.
IN_GIT = os.path.isdir(os.path.join(ROOT, ".git")) or os.path.isfile(os.path.join(ROOT, ".git"))

DOCS = {
    "NORTH-STAR.md": "The north star: what the partners asked for, what makes Onyx new, and the build list",
    "TRACKER.md": "The vision, every idea, and the decision log",
    "contributor-loop.md": "The contributor loop (CL1-CL8): easy data contribution, one system for users",
    "north-star-loop.md": "The north star work loop (NL0-NL5): items, results, log",
    "agents.md": "Connect an AI agent: find, read, propose and check Onyx data over MCP (NL21)",
    "join.md": "Join a round as an independent observer: the join kit, from key to sealed reveal (NL16)",
    "pre-public-loop.md": "The pre-public work loop (PP1-PP7): items, results, log",
    "website-plan.md": "The website plan and its W-numbered rounds",
    "adr": "Architecture decision records, one per decision (index: DECISIONS.md)",
    "api": "The API description (OpenAPI 3.1), checked against the code",
    "audit": "Dated audit and review reports",
    "ops": "Running Onyx: configuration, deployment, rehearsals, runbooks, launch checklist",
    "partners": "Partner call notes, the partner tracker (T1-T14) and design proposals",
    "research": "Research notes behind a decision",
    "sectors": "How each sector (crypto, equities, macro, commodities, prediction markets) maps onto Onyx",
    "spec": "The protocol specification, one file per part",
}

SCRIPT_GROUPS = [
    ("Checks (run by check-all or check-docs)", r"^check-|^threats-legacy\.txt$"),
    ("Builds", r"^build-"),
    ("Rehearsals and demos", r"^(demo-|level2-|level3-|track-|preview-bundle|docker-smoke|soak|smoke-lock)"),
    ("Independent recounts (Python, not the Rust recipes)", r"^recount-"),
    ("Data fetches and refreshes", r"^(fetch-|refresh-|record-|read-indexer|stream-|reserve-live)"),
    ("Audits, tests of scripts, operations", r"."),
]


def read(p):
    with open(os.path.join(ROOT, p), encoding="utf-8") as f:
        return f.read()


def cargo_desc(d):
    p = os.path.join(ROOT, d, "Cargo.toml")
    if not os.path.exists(p):
        return None
    m = re.search(r'^description\s*=\s*"(.*)"\s*$', read(os.path.join(d, "Cargo.toml")), re.M)
    return m.group(1) if m else None


def first_sentence(s):
    """The first sentence, not cut at an abbreviation (U.S., e.g.) and without a TESTNET ONLY tag."""
    s = re.sub(r"^TESTNET ONLY\.\s*", "", s.strip())
    for m in re.finditer(r"[.!?](?=\s|$)", s):
        head = s[: m.end()]
        if re.search(r"(\b[A-Z]\.){2,}$|\b(e\.g|i\.e|etc|vs)\.$", head):
            continue
        return head.rstrip(".")
    return s.rstrip(".")


def contract_notes():
    out = []
    for f in sorted(os.listdir(os.path.join(ROOT, "contracts/src"))):
        if not f.endswith(".sol"):
            continue
        text = read(f"contracts/src/{f}")
        m = re.search(r"^/// @title (\w+)\n((?:/// .*\n)*)", text, re.M)
        if m:
            notice = " ".join(l[4:] for l in m.group(2).splitlines())
            notice = re.sub(r"^@notice\s*", "", notice.strip())
            out.append((m.group(1), f, first_sentence(notice)))
        else:
            m = re.search(r"^/// @notice (.*)\n(?:/// .*\n)*interface (\w+)", text, re.M)
            if m:
                out.append((m.group(2), f, first_sentence(m.group(1))))
    return out


def layout(errors):
    rows = []
    groups = [("crates", "Core (sector-agnostic; never imports an adapter)"), ("adapters", "Sector adapters and schemas"),
              ("apps", "Apps"), ("recipes", "Recipes (WebAssembly, outside the workspace)"), ("experiments", "Experiments")]
    for top, title in groups:
        rows.append(f"| **{title}** | |")
        for name in sorted(os.listdir(os.path.join(ROOT, top))):
            d = f"{top}/{name}"
            if not os.path.isdir(os.path.join(ROOT, d)):
                continue
            desc = FOLDERS.get(d) or cargo_desc(d)
            if not desc:
                errors.append(f"{d}: no description (add one to its Cargo.toml or FOLDERS in scripts/check-readme.py)")
                continue
            rows.append(f"| `{d}` | {first_sentence(desc)} |")
            if d == "apps/web" and cargo_desc("apps/web/verify"):
                rows.append(f"| `apps/web/verify` | {first_sentence(cargo_desc('apps/web/verify'))} |")
    rows.append("| **Everything else** | |")
    for name in sorted(os.listdir(ROOT)):
        p = os.path.join(ROOT, name)
        if name.startswith(".") or name in SKIP_TOP or not os.path.isdir(p) or name in ("crates", "adapters", "apps", "recipes", "experiments"):
            continue
        if IN_GIT and not os.popen(f"git -C {ROOT} ls-files -- {name} | head -1").read().strip():
            continue  # untracked build output
        if name not in FOLDERS:
            errors.append(f"{name}/: a top-level folder with no description in scripts/check-readme.py")
            continue
        rows.append(f"| `{name}/` | {FOLDERS[name]} |")
    table = "| Path | What |\n|---|---|\n" + "\n".join(rows)
    cs = contract_notes()
    ctable = "| Contract | What |\n|---|---|\n" + "\n".join(f"| [`{n}`](contracts/src/{f}) | {d} |" for n, f, d in cs)
    return table + "\n\nContracts (`contracts/src`, Base Sepolia and local Anvil only):\n\n" + ctable


def routes():
    spec = json.loads(read("docs/api/openapi.json"))
    rows = ["| Method | Route | What | Paid |", "|---|---|---|---|"]
    for path, item in spec["paths"].items():
        for m in ("get", "post"):
            op = item.get(m)
            if op:
                rows.append(f"| `{m.upper()}` | `{path}` | {op['summary']} | {'x402' if op.get('x-onyx-paid') else 'free'} |")
    return "\n".join(rows)


def script_desc(name):
    p = f"scripts/{name}"
    if os.path.isdir(os.path.join(ROOT, p)):
        files = sorted(os.listdir(os.path.join(ROOT, p)))
        return f"Folder: {', '.join(files)}"
    if name.endswith(".txt"):
        return "Data the checks read"
    lines = read(p).splitlines()
    text, started = [], False
    for l in lines[:20]:
        raw = l.strip()
        if not started and (not raw or raw.startswith("#!") or "SPDX" in raw or raw.startswith(("set -", "import ", "from "))):
            continue
        s2 = re.sub(r'^(#+|"""|\'\'\'|//)\s*', "", raw).rstrip('"').strip()
        if started and (not s2 or not re.match(r'^(#|"""|//)', raw) and not text[-1:] == [""]):
            # A blank line, or code after a comment, ends the header's first paragraph.
            if not s2 or not raw.startswith(("#", "//")):
                break
        if not s2:
            if started:
                break
            continue
        started = True
        text.append(s2)
        if re.search(r"[.!?:]$", s2) and len(" ".join(text)) > 40:
            break
    if text:
        return first_sentence(" ".join(text)).rstrip(":")
    return None


def scripts_md(errors):
    names = sorted(n for n in os.listdir(os.path.join(ROOT, "scripts")) if n not in ("__pycache__", "README.md"))

    placed = {}
    for n in names:
        for title, pat in SCRIPT_GROUPS:
            if re.search(pat, n):
                placed.setdefault(title, []).append(n)
                break
    out = ["# Scripts", "", "Generated by `scripts/check-readme.py --write` from each script's own header comment;",
           "`scripts/check-docs.sh` fails when this file is stale. Run every script from the repository root.", ""]
    for title, _ in SCRIPT_GROUPS:
        if title not in placed:
            continue
        out += [f"## {title}", "", "| Script | What |", "|---|---|"]
        for n in placed[title]:
            d = script_desc(n)
            if not d:
                errors.append(f"scripts/{n}: no header comment saying what it does")
                d = "(no description)"
            out.append(f"| [`{n}`]({n}) | {d.replace('|', '/')} |")
        out.append("")
    return "\n".join(out).rstrip() + "\n"


def docs_block(errors):
    rows = ["| Path | What |", "|---|---|"]
    for n in sorted(os.listdir(os.path.join(ROOT, "docs"))):
        if n == "README.md":
            continue
        if n not in DOCS:
            errors.append(f"docs/{n}: no description in scripts/check-readme.py DOCS")
            continue
        link = f"{n}/" if os.path.isdir(os.path.join(ROOT, "docs", n)) else n
        rows.append(f"| [`{link}`]({link}) | {DOCS[n]} |")
    return "\n".join(rows)


def put(text, name, body):
    pat = re.compile(r"(<!-- generated:%s[^>]*-->\n)(.*?)(<!-- /generated:%s -->)" % (name, name), re.S)
    if not pat.search(text):
        raise SystemExit(f"check-readme: block {name} missing")
    return pat.sub(lambda m: m.group(1) + body + "\n" + m.group(3), text)


def build():
    errors = []
    readme = read("README.md")
    readme = put(readme, "layout", layout(errors))
    readme = put(readme, "routes", routes())
    docs = put(read("docs/README.md"), "docs", docs_block(errors))
    return {"README.md": readme, "scripts/README.md": scripts_md(errors), "docs/README.md": docs}, errors


def main():
    want, errors = build()
    if "--write" in sys.argv:
        for p, t in want.items():
            with open(os.path.join(ROOT, p), "w", encoding="utf-8") as f:
                f.write(t)
    stale = [p for p, t in want.items() if not os.path.exists(os.path.join(ROOT, p)) or read(p) != t]
    for e in errors:
        print(f"readme: {e}")
    for p in stale:
        print(f"readme: {p} is stale: run scripts/check-readme.py --write")
    if errors or stale:
        sys.exit(1)
    print(f"readme ok: README layout, routes, scripts/README.md and docs/README.md are current")


def self_test():
    want, errors = build()
    if errors:
        raise SystemExit(f"check-readme self-test: real tree has errors: {errors}")
    bad = []
    if "onyx-api" not in want["README.md"] or "/v1/openapi.json" not in want["README.md"]:
        bad.append("layout or routes missing a known entry")
    global FOLDERS
    saved = FOLDERS
    FOLDERS = {k: v for k, v in saved.items() if k != "deploy"}
    if not build()[1]:
        bad.append("an undescribed top-level folder passed")
    FOLDERS = saved
    if bad:
        raise SystemExit(f"check-readme self-test failed: {bad}")
    print("readme self-test ok: an undescribed folder fails")


if __name__ == "__main__":
    self_test() if "--self-test" in sys.argv else main()
