#!/usr/bin/env python3
"""Every file the server's release build reads at compile time is in the Docker build context.

`.dockerignore` is an allowlist (`*`, then `!` lines): a file it leaves out is missing inside
`docker build`, and `cargo build` there fails, while every build on the host still passes. That
happened once: `include_str!("../../../docs/api/openapi.json")` in onyx-api (2026-10-25) broke
the image for two weeks, found by scripts/docker-smoke.sh (THREATS DOCKER-CTX-1; this form after
review DOCKER-CTX-R1). This check needs no Docker. Over every workspace member and path dependency (from the Cargo.toml files) it
requires in the context:
  - each crate's Cargo.toml;
  - each source file of its library, binaries and build script, found by following `mod`
    declarations from the crate roots as rustc does (`#[path]` too; a module under an exact
    `#[cfg(test)]` is test code and is not followed);
  - each file those sources name in `include_str!`, `include_bytes!` or `include!` (plain and
    raw string literals; any other argument, such as `concat!` or a macro variable, fails, since
    the check cannot read it).
Tests, examples and benches are not followed (`cargo build` does not compile them). Inline
`#[cfg(test)] mod name { ... }` blocks are skipped; a file module declared inside another inline
module fails (not modelled). `.dockerignore` lines are matched as Docker matches them (`*` within
one path segment, `**/` any depth, a directory rule covers what is under it, the last match
decides); a `!` line with a wildcard before its last `/` is refused (the classic builder may not
descend into an excluded directory for it).

  scripts/check-docker-context.py               check the repository (exit 1 on a miss)
  scripts/check-docker-context.py --self-test   planted misses must fail, test-only code must pass
"""

import glob
import os
import re
import shutil
import sys
import tempfile
import tomllib

# ----------------------------------------------------------------- .dockerignore, as Docker reads it


def docker_regex(pat):
    """moby/patternmatcher's translation: `**` any depth, `*` and `?` inside one segment."""
    out, i = "^", 0
    while i < len(pat):
        c = pat[i]
        if c == "*":
            if pat[i + 1 : i + 2] == "*":
                while pat[i + 1 : i + 2] == "*":
                    i += 1
                if i + 1 >= len(pat):
                    out += ".*"
                else:
                    if pat[i + 1] == "/":
                        i += 1
                    out += "(.*/)?"
            else:
                out += "[^/]*"
        elif c == "?":
            out += "[^/]"
        elif c == "\\" and i + 1 < len(pat):
            i += 1
            out += re.escape(pat[i])
        elif c == "[":
            j = pat.find("]", i + 1)
            if j < 0:
                raise ValueError(f"unclosed [ in .dockerignore pattern {pat!r}")
            out += pat[i : j + 1]
            i = j
        else:
            out += re.escape(c)
        i += 1
    return re.compile(out + "$")


def rules(root):
    """The .dockerignore lines as (keep, regex, text), in order; refuses lines the check cannot judge."""
    out = []
    with open(os.path.join(root, ".dockerignore"), encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            keep = line.startswith("!")
            pat = line[1:].strip() if keep else line
            pat = os.path.normpath(pat).replace(os.sep, "/").lstrip("/")
            if keep and re.search(r"[*?\[]", pat.rsplit("/", 1)[0] if "/" in pat else ""):
                raise ValueError(
                    f".dockerignore line {line!r}: a `!` pattern with a wildcard before its last `/` "
                    "cannot be judged (the classic builder may skip the directory); name the directory"
                )
            out.append((keep, docker_regex(pat), line))
    return out


def admitted(rule_list, rel):
    """Whether `rel` is in the context: a rule matches the path or one of its parent directories;
    the last matching rule decides; nothing matching means kept."""
    parts = rel.split("/")
    paths = ["/".join(parts[: i + 1]) for i in range(len(parts))]
    keep = True
    for k, rx, _ in rule_list:
        if any(rx.match(p) for p in paths):
            keep = k
    return keep


# ----------------------------------------------------------------- Rust source, lightly lexed


RAW = re.compile(r'b?r(#*)"')


def lex(text):
    """(code, skel): comments blanked in both; string and char contents also blanked in skel, so
    braces and keywords inside literals do not count. Lengths match the input."""
    code, skel, i, n = list(text), list(text), 0, len(text)

    def blank(a, b, both):
        for k in range(a, b):
            if text[k] != "\n":
                skel[k] = " "
                if both:
                    code[k] = " "

    while i < n:
        c = text[i]
        if text.startswith("//", i):
            j = text.find("\n", i)
            j = n if j < 0 else j
            blank(i, j, True)
            i = j
        elif text.startswith("/*", i):
            depth, j = 1, i + 2
            while j < n and depth:
                if text.startswith("/*", j):
                    depth, j = depth + 1, j + 2
                elif text.startswith("*/", j):
                    depth, j = depth - 1, j + 2
                else:
                    j += 1
            blank(i, j, True)
            i = j
        elif c in "br" and (m := RAW.match(text, i)) and (i == 0 or not (text[i - 1].isalnum() or text[i - 1] == "_")):
            end = text.find('"' + m.group(1), i + m.end())
            end = n if end < 0 else end + 1 + len(m.group(1))
            blank(i + m.end(), end - 1 - len(m.group(1)), False)
            i = end
        elif c == '"':
            j = i + 1
            while j < n and text[j] != '"':
                j += 2 if text[j] == "\\" else 1
            blank(i + 1, j, False)
            i = j + 1
        elif c == "'":
            if text[i + 1 : i + 2] == "\\":
                j = text.find("'", i + 2)
                j = n if j < 0 else j
                blank(i + 1, j, False)
                i = j + 1
            elif i + 2 < n and text[i + 2] == "'":
                blank(i + 1, i + 2, False)
                i += 3
            else:
                i += 1  # a lifetime
        else:
            i += 1
    return "".join(code), "".join(skel)


ATTRS = r"((?:#\[[^\]]*\]\s*)*)"
VIS = r"(?:pub(?:\s*\([^)]*\))?\s+)?"
MOD_DECL = re.compile(ATTRS + VIS + r"\bmod\s+(?:r#)?([A-Za-z_]\w*)\s*([;{])")
INCLUDE = re.compile(r"\binclude(?:_str|_bytes)?!\s*[(\[{]\s*")
PATH_ATTR = re.compile(r'#\[\s*path\s*=\s*"([^"]*)"\s*\]')
CFG_TEST = re.compile(r"#\[\s*cfg\s*\(\s*test\s*\)\s*\]")


def close_brace(skel, open_at):
    depth = 0
    for k in range(open_at, len(skel)):
        if skel[k] == "{":
            depth += 1
        elif skel[k] == "}":
            depth -= 1
            if depth == 0:
                return k
    return len(skel) - 1


def literal_at(code, k):
    """The string literal at code[k:] (plain or raw), unescaped, or None."""
    m = RAW.match(code, k)
    if m and code[k] == "r":
        end = code.find('"' + m.group(1), k + m.end())
        return None if end < 0 else code[k + m.end() : end]
    if code[k : k + 1] != '"':
        return None
    j, out = k + 1, []
    while j < len(code) and code[j] != '"':
        if code[j] == "\\":
            esc = code[j + 1 : j + 2]
            simple = {"n": "\n", "t": "\t", "r": "\r", "0": "\0", "\\": "\\", '"': '"', "'": "'"}
            if esc in simple:
                out.append(simple[esc])
                j += 2
                continue
            return None  # \x, \u{..}, line continuation: not in a path we expect; fail loudly
        out.append(code[j])
        j += 1
    return "".join(out)


# ----------------------------------------------------------------- the walk


class Check:
    def __init__(self, root):
        self.root = os.path.abspath(root)
        self.rules = rules(self.root)
        self.problems = []
        self.includes = 0
        self.sources = set()

    def rel(self, path):
        return os.path.relpath(path, self.root).replace(os.sep, "/")

    def need(self, path, what, where):
        rel = self.rel(path)
        if rel.startswith("../"):
            self.problems.append(f"{where}: {what} {rel} is outside the repository")
        elif not os.path.isfile(path):
            self.problems.append(f"{where}: {what} {rel} does not exist")
        elif not admitted(self.rules, rel):
            self.problems.append(f"{where}: {what} {rel} is left out of the build context by .dockerignore (add a `!` line for it)")
            return False
        return True

    def crates(self):
        """Workspace members and every path dependency, by directory."""
        top = tomllib.load(open(os.path.join(self.root, "Cargo.toml"), "rb"))
        ws = top.get("workspace", {})
        excluded = {os.path.normpath(os.path.join(self.root, e)) for e in ws.get("exclude", [])}
        todo = []
        for m in ws.get("members", []):
            hits = sorted(glob.glob(os.path.join(self.root, m))) or [os.path.join(self.root, m)]
            todo += [os.path.normpath(h) for h in hits if os.path.normpath(h) not in excluded]
        for spec in ws.get("dependencies", {}).values():
            if isinstance(spec, dict) and "path" in spec:
                todo.append(os.path.normpath(os.path.join(self.root, spec["path"])))
        seen, order = set(), []
        while todo:
            d = todo.pop(0)
            if d in seen:
                continue
            seen.add(d)
            order.append(d)
            manifest = os.path.join(d, "Cargo.toml")
            if not self.need(manifest, "crate manifest", "Cargo.toml workspace"):
                continue
            c = tomllib.load(open(manifest, "rb"))
            tables = [c.get(k, {}) for k in ("dependencies", "dev-dependencies", "build-dependencies")]
            for t in c.get("target", {}).values():
                tables += [t.get(k, {}) for k in ("dependencies", "dev-dependencies", "build-dependencies")]
            for t in tables:
                for spec in t.values():
                    if isinstance(spec, dict) and "path" in spec:
                        todo.append(os.path.normpath(os.path.join(d, spec["path"])))
        return order

    def roots(self, d):
        c = tomllib.load(open(os.path.join(d, "Cargo.toml"), "rb"))
        out = []
        lib = c.get("lib", {}).get("path", "src/lib.rs")
        if os.path.isfile(os.path.join(d, lib)) or "lib" in c:
            out.append(os.path.join(d, lib))
        bins = c.get("bin", [])
        for b in bins:
            if "path" in b:
                out.append(os.path.join(d, b["path"]))
            elif b.get("name") and os.path.isfile(os.path.join(d, "src/bin", b["name"] + ".rs")):
                out.append(os.path.join(d, "src/bin", b["name"] + ".rs"))
        if c.get("package", {}).get("autobins", True):
            if os.path.isfile(os.path.join(d, "src/main.rs")):
                out.append(os.path.join(d, "src/main.rs"))
            out += sorted(glob.glob(os.path.join(d, "src/bin/*.rs")))
            out += sorted(glob.glob(os.path.join(d, "src/bin/*/main.rs")))
        build = c.get("package", {}).get("build", None)
        if isinstance(build, str):
            out.append(os.path.join(d, build))
        elif build is not False and os.path.isfile(os.path.join(d, "build.rs")):
            out.append(os.path.join(d, "build.rs"))
        return sorted(set(os.path.normpath(p) for p in out))

    def source(self, path, mod_rs, where):
        path = os.path.normpath(path)
        if path in self.sources:
            return
        self.sources.add(path)
        if not self.need(path, "source file", where):
            return
        text = open(path, encoding="utf-8").read()
        code, skel = lex(text)
        rel = self.rel(path)
        here = os.path.dirname(path)
        stem = os.path.basename(path)[:-3]
        base = here if mod_rs else os.path.join(here, stem)
        # Inline modules: test blocks are blanked; a file module inside a non-test one is refused.
        blanked = []
        for m in MOD_DECL.finditer(skel):
            if m.group(3) != "{":
                continue
            end = close_brace(skel, m.end() - 1)
            if CFG_TEST.search(m.group(1)):
                blanked.append((m.start(), end))
            else:
                inner = skel[m.end() : end]
                if re.search(r"\bmod\s+(?:r#)?[A-Za-z_]\w*\s*;", inner):
                    line = skel.count("\n", 0, m.start()) + 1
                    self.problems.append(f"{rel}:{line}: a file module inside the inline module {m.group(2)} (not modelled by this check)")
        def live(pos):
            return not any(a <= pos <= b for a, b in blanked)
        for m in MOD_DECL.finditer(skel):
            if m.group(3) != ";" or not live(m.start()):
                continue
            attrs = code[m.start(1) : m.end(1)]
            if CFG_TEST.search(attrs):
                continue
            line = skel.count("\n", 0, m.start()) + 1
            name = m.group(2)
            p = PATH_ATTR.search(attrs)
            if p:
                target = os.path.join(here, p.group(1))
                self.source(target, os.path.basename(target) == "mod.rs", f"{rel}:{line}")
                continue
            cands = [os.path.join(base, name + ".rs"), os.path.join(base, name, "mod.rs")]
            hit = next((c for c in cands if os.path.isfile(c)), None)
            if hit is None:
                self.problems.append(f"{rel}:{line}: no file for `mod {name};` ({self.rel(cands[0])} or {self.rel(cands[1])})")
                continue
            self.source(hit, hit.endswith("mod.rs"), f"{rel}:{line}")
        for m in INCLUDE.finditer(skel):
            if not live(m.start()):
                continue
            line = skel.count("\n", 0, m.start()) + 1
            lit = literal_at(code, m.end())
            if lit is None:
                arg = code[m.end() : m.end() + 40].split("\n")[0]
                self.problems.append(f"{rel}:{line}: an include whose path is not a plain string literal ({arg}...) cannot be checked")
                continue
            self.includes += 1
            self.need(os.path.join(here, lit), "include", f"{rel}:{line}")

    def run(self):
        for d in self.crates():
            if not os.path.isfile(os.path.join(d, "Cargo.toml")):
                continue
            for r in self.roots(d):
                self.source(r, True, f"crate root of {self.rel(d)}")
        return self


# ----------------------------------------------------------------- self-test


def self_test():
    src = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    failures = []

    def tree(t, files, extra_ignore=""):
        shutil.copy(os.path.join(src, ".dockerignore"), os.path.join(t, ".dockerignore"))
        if extra_ignore:
            with open(os.path.join(t, ".dockerignore"), "a") as f:
                f.write(extra_ignore)
        for rel, body in files.items():
            p = os.path.join(t, rel)
            os.makedirs(os.path.dirname(p), exist_ok=True)
            open(p, "w").write(body)

    base = {
        "Cargo.toml": '[workspace]\nmembers = ["crates/demo"]\n',
        "crates/demo/Cargo.toml": '[package]\nname = "demo"\nversion = "0.0.0"\n',
        "crates/demo/kept.txt": "x\n",
        "docs/left-out.json": "{}\n",
        "crates/demo/src/lib.rs": (
            '// include_str!("../../../docs/left-out.json") in a comment\n'
            'pub const K: &str = include_str!("../kept.txt");\n'
            'pub const U: &str = "https://x/include_str!(";\n'
            "#[cfg(test)]\nmod only_tests;\n#[cfg(test)]\nmod tests;\n"
            "#[cfg(test)]\nmod inline {\n    const T: &str = include_str!(\"../../../docs/left-out.json\");\n}\n"
            "pub mod part;\n"
        ),
        "crates/demo/src/part.rs": "pub fn f() {}\n",
        "crates/demo/src/only_tests.rs": 'const T: &str = include_str!("../../../docs/left-out.json");\n',
        "crates/demo/src/tests.rs": 'const T: &str = include_str!("../../../docs/left-out.json");\n',
        "crates/demo/tests/it.rs": 'const T: &str = include_str!("../../../docs/left-out.json");\n',
        "crates/demo/src/orphan_tests.rs": 'const T: &str = include_str!("../../../docs/left-out.json");\n',
    }
    with tempfile.TemporaryDirectory() as t:
        tree(t, base)
        c = Check(t).run()
        if c.problems or c.includes != 1:
            failures.append(f"a clean tree gave {c.problems} ({c.includes} includes)")

    left = 'pub const S: &[u8] = {};\n'
    planted = {
        "a release include of a docs file (the 2026-10-25 shape)": {"crates/demo/src/part.rs": 'pub const S: &str = include_str!("../../../docs/left-out.json");\n'},
        "a raw-string include": {"crates/demo/src/part.rs": left.format('include_bytes!(r#"../../../docs/left-out.json"#)')},
        "a bracket-delimited include": {"crates/demo/src/part.rs": left.format('include_bytes![ "../../../docs/left-out.json" ]')},
        "a computed include": {"crates/demo/src/part.rs": 'pub const C: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/x"));\n'},
        "a macro-variable include": {"crates/demo/src/part.rs": "macro_rules! m { ($p:literal) => { include_str!($p) }; }\n"},
        "a #[path] module outside the context": {"crates/demo/src/part.rs": '#[path = "../../../docs/gen.rs"]\nmod gen;\n', "docs/gen.rs": "\n"},
        "a module directory named target": {"crates/demo/src/part.rs": "mod target;\n", "crates/demo/src/target/mod.rs": "\n"},
        "a commented cfg(test) before a real module": {"crates/demo/src/part.rs": "// #[cfg(test)]\nmod spec;\n", "crates/demo/src/spec.rs": 'const S: &str = include_str!("../../../docs/left-out.json");\n'},
        "a `pub mod tests;` compiled into the library": {"crates/demo/src/part.rs": "pub mod tests;\n", "crates/demo/src/part/tests.rs": 'const S: &str = include_str!("../../../../docs/left-out.json");\n'},
        "a workspace member outside the context": {"Cargo.toml": '[workspace]\nmembers = ["crates/demo", "tools/x"]\n', "tools/x/Cargo.toml": '[package]\nname = "x"\nversion = "0.0.0"\n', "tools/x/src/lib.rs": "\n"},
        "a build script include": {"crates/demo/build.rs": 'fn main() { let _ = include_str!("../../docs/left-out.json"); }\n'},
    }
    for name, files in planted.items():
        with tempfile.TemporaryDirectory() as t:
            tree(t, {**base, **files})
            c = Check(t).run()
            if not c.problems:
                failures.append(f"{name} passed")
    # Docker's matcher: `*` stays inside one segment.
    with tempfile.TemporaryDirectory() as t:
        tree(t, base, "!docs/*.json\n")
        r = rules(t)
        if not admitted(r, "docs/left-out.json") or admitted(r, "docs/api/x.json"):
            failures.append("`!docs/*.json` was not matched as Docker matches it")
        tree(t, base, "!docs/*/x.json\n")
        try:
            rules(t)
            failures.append("a `!` line with a wildcard directory was not refused")
        except ValueError:
            pass
    if failures:
        for f in failures:
            print("SELF-TEST FAILED:", f)
        return 1
    print(f"check-docker-context self-test ok: {len(planted)} planted misses fail, test-only code passes, Docker's matcher holds")
    return 0


def main():
    if sys.argv[1:] == ["--self-test"]:
        return self_test()
    if sys.argv[1:]:
        print(__doc__)
        return 2
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    try:
        c = Check(root).run()
    except ValueError as e:
        print("DOCKER CONTEXT:", e)
        return 1
    for p in c.problems:
        print("DOCKER CONTEXT:", p)
    if c.problems:
        return 1
    print(f"docker context ok: {len(c.sources)} source files and {c.includes} compile-time includes, all in the build context")
    return 0


if __name__ == "__main__":
    sys.exit(main())
