#!/usr/bin/env python3
"""docs/api/openapi.json against the code (pre-public loop PP6). Run by scripts/check-docs.sh.

1. Shape: OpenAPI 3.1.0; every $ref resolves; operation ids are unique; every `{param}` of a path
   template is a path parameter of each of its operations, and no other path parameter is listed;
   every operation has a success response.
2. Routes: the `.route("PATH", METHOD(..))` calls in crates/onyx-api/src (tests excluded) are
   exactly the spec's paths and methods (HEAD is implied by GET), both ways.
3. Error codes: every `StatusCode::X, "code"` in crates/onyx-api/src is in `x-onyx-error-codes`
   with that status, and every listed code is in the code (or is a proposal validation code that
   onyx_intake's `SubmissionError::code` / `AnswersError::code` returns, listed at 400).
4. Docs: every listed code is in the error table of docs/spec/delivery.md; every spec path is
   reachable through deploy/caddy/Caddyfile's matchers with its method.
5. The website's copy (apps/web/static/openapi.json) and the agent toolkit's copy
   (apps/agent/openapi.json, NL21) are byte-identical to the spec.

`--self-test` plants one fault per rule in a copy and requires each to fail.
"""
import copy, glob, json, os, re, sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SPEC = os.path.join(ROOT, "docs/api/openapi.json")
METHODS = {"get", "post", "put", "patch", "delete"}
STATUS = {"BAD_REQUEST": 400, "PAYMENT_REQUIRED": 402, "FORBIDDEN": 403, "NOT_FOUND": 404, "METHOD_NOT_ALLOWED": 405,
          "CONFLICT": 409, "PAYLOAD_TOO_LARGE": 413, "UNPROCESSABLE_ENTITY": 422, "TOO_MANY_REQUESTS": 429,
          "UNAVAILABLE_FOR_LEGAL_REASONS": 451, "INTERNAL_SERVER_ERROR": 500, "SERVICE_UNAVAILABLE": 503,
          "GATEWAY_TIMEOUT": 504}


def read(p):
    with open(os.path.join(ROOT, p), encoding="utf-8") as f:
        return f.read()


def api_sources():
    out = {}
    for f in sorted(glob.glob(os.path.join(ROOT, "crates/onyx-api/src/*.rs"))):
        if f.endswith("tests.rs"):
            continue
        with open(f, encoding="utf-8") as h:
            text = h.read()
        # Drop `#[cfg(test)] mod tests { ... }` tails (inline test modules).
        out[os.path.relpath(f, ROOT)] = re.split(r"#\[cfg\(test\)\]\s*mod\s+\w+\s*\{", text)[0]
    return out


def code_routes(src):
    routes = set()
    for text in src.values():
        for m in re.finditer(r'\.route\(\s*"(/[^"]*)"\s*,\s*([a-z]+)\(', text):
            routes.add((m.group(1), m.group(2)))
    return routes


def code_errors(src):
    errs = {}
    for f, text in src.items():
        for m in re.finditer(r"StatusCode::([A-Z_]+)\s*,\s*\"([a-z0-9_]+)\"", text):
            errs.setdefault(m.group(2), set()).add(STATUS.get(m.group(1), m.group(1)))
        # Closures that take the code (`let refused = |code, m| ApiError::new(StatusCode::X, code, m)`),
        # then their calls with a literal code.
        for m in re.finditer(r"let (\w+) = \|code\b[^|]*\|[^;]*?ApiError::new\(\s*StatusCode::([A-Z_]+)\s*,\s*code\b", text, re.S):
            for c in re.finditer(r"\b" + m.group(1) + r"\(\s*\"([a-z0-9_]+)\"", text):
                errs.setdefault(c.group(1), set()).add(STATUS.get(m.group(2), m.group(2)))
    return errs


def intake_codes():
    codes = set()
    for f, enum in [("crates/onyx-intake/src/submission.rs", "SubmissionError"), ("crates/onyx-intake/src/answers.rs", "AnswersError")]:
        text = read(f)
        m = re.search(r"impl " + enum + r" \{.*?pub fn code\(&self\) -> &'static str \{(.*?)\n    \}", text, re.S)
        if not m:
            raise SystemExit(f"check-openapi: no {enum}::code in {f}")
        codes |= set(re.findall(r'=> "([a-z0-9_]+)"', m.group(1)))
    return codes


def resolve(spec, ref):
    node = spec
    for part in ref.lstrip("#/").split("/"):
        if not isinstance(node, dict) or part not in node:
            return None
        node = node[part]
    return node


def walk_refs(node):
    if isinstance(node, dict):
        for k, v in node.items():
            if k == "$ref":
                yield v
            else:
                yield from walk_refs(v)
    elif isinstance(node, list):
        for v in node:
            yield from walk_refs(v)


def caddy_allows(caddy, path, method):
    """Whether a matcher block names `method` and a path pattern covering `path`."""
    concrete = re.sub(r"\{[^}]+\}", "x", path)
    for block in re.findall(r"@\w+\s*\{(.*?)\}", caddy, re.S):
        ms = re.search(r"^\s*method\s+(.+)$", block, re.M)
        ps = re.search(r"^\s*path\s+(.+)$", block, re.M)
        if not ms or not ps or method.upper() not in ms.group(1).split():
            continue
        for pat in ps.group(1).split():
            if pat == concrete or (pat.endswith("*") and concrete.startswith(pat[:-1])):
                return True
    return False


def check(spec, src, intake, delivery, caddy, site_copy, spec_bytes):
    bad = []
    if spec.get("openapi") != "3.1.0":
        bad.append("openapi must be 3.1.0")
    for ref in set(walk_refs(spec)):
        if not ref.startswith("#/") or resolve(spec, ref) is None:
            bad.append(f"unresolved $ref {ref}")
    ids = []
    spec_routes = set()
    for path, item in spec.get("paths", {}).items():
        want = set(re.findall(r"\{([^}]+)\}", path))
        for method, op in item.items():
            if method not in METHODS:
                continue
            spec_routes.add((path, method))
            ids.append(op.get("operationId"))
            got = set()
            for prm in op.get("parameters", []):
                prm = resolve(spec, prm["$ref"]) if "$ref" in prm else prm
                if prm and prm.get("in") == "path":
                    got.add(prm["name"])
            if got != want:
                bad.append(f"{method.upper()} {path}: path parameters {sorted(got)} != template {sorted(want)}")
            if not any(c.startswith("2") for c in op.get("responses", {})):
                bad.append(f"{method.upper()} {path}: no success response")
    dup = {i for i in ids if ids.count(i) > 1 or i is None}
    if dup:
        bad.append(f"operationId missing or repeated: {sorted(map(str, dup))}")
    routes = code_routes(src)
    for r in sorted(routes - spec_routes):
        bad.append(f"route {r[1].upper()} {r[0]} is in the code, not in the spec")
    for r in sorted(spec_routes - routes):
        bad.append(f"route {r[1].upper()} {r[0]} is in the spec, not in the code")
    listed = spec.get("x-onyx-error-codes", {})
    errs = code_errors(src)
    for c, statuses in sorted(errs.items()):
        if c not in listed:
            bad.append(f"error code {c} ({sorted(statuses)}) is in the code, not in x-onyx-error-codes")
        elif statuses != {listed[c]["status"]}:
            bad.append(f"error code {c}: code status {sorted(statuses)} != spec {listed[c]['status']}")
    for c, v in sorted(listed.items()):
        if c in errs:
            continue
        if c in intake:
            if v["status"] != 400:
                bad.append(f"proposal code {c} must be listed at 400")
        else:
            bad.append(f"error code {c} is in x-onyx-error-codes, not in the code")
    for c in sorted(intake - set(listed)):
        bad.append(f"proposal code {c} (onyx_intake) is not in x-onyx-error-codes")
    table = delivery.split("### Error codes", 1)[-1].split("\n## ", 1)[0]
    for c in sorted(set(listed) - intake):
        if f"`{c}`" not in table:
            bad.append(f"error code {c} is not in docs/spec/delivery.md's error table")
    for path, method in sorted(spec_routes):
        if not caddy_allows(caddy, path, method):
            bad.append(f"{method.upper()} {path} is not reachable through deploy/caddy/Caddyfile")
    if site_copy != spec_bytes:
        bad.append("apps/web/static/openapi.json differs from docs/api/openapi.json (copy it)")
    return bad


def inputs():
    spec_bytes = read("docs/api/openapi.json")
    site = os.path.join(ROOT, "apps/web/static/openapi.json")
    site_copy = open(site, encoding="utf-8").read() if os.path.exists(site) else None
    return (json.loads(spec_bytes), api_sources(), intake_codes(), read("docs/spec/delivery.md"),
            read("deploy/caddy/Caddyfile"), site_copy, spec_bytes)


def self_test():
    spec, src, intake, delivery, caddy, site_copy, spec_bytes = inputs()
    if check(spec, src, intake, delivery, caddy, site_copy, spec_bytes):
        raise SystemExit("check-openapi self-test: the real inputs must pass first")
    faults = []

    def plant(name, f):
        args = [copy.deepcopy(spec), dict(src), set(intake), delivery, caddy, site_copy, spec_bytes]
        f(args)
        if not check(*args):
            faults.append(name)

    def drop_path(a): a[0]["paths"].pop("/v1/health")
    def extra_route(a): a[1]["x.rs"] = '.route("/v1/hidden", get(h))'
    def bad_ref(a): a[0]["paths"]["/v1/health"]["get"]["responses"]["200"]["content"]["application/json"]["schema"] = {"$ref": "#/components/schemas/Nope"}
    def new_code(a): a[1]["y.rs"] = 'ApiError::new(StatusCode::CONFLICT, "brand_new", "x")'
    def wrong_status(a): a[0]["x-onyx-error-codes"]["timeout"]["status"] = 503
    def stale_code(a): a[0]["x-onyx-error-codes"]["gone_code"] = {"status": 400, "description": "x"}
    def intake_new(a): a[2].add("new_intake_code")
    def doc_table(a): a[3] = a[3].replace("`timeout`", "timeout")
    def caddy_gap(a): a[4] = a[4].replace("/v1/served/*", "")
    def site_drift(a): a[5] = (a[5] or "") + " "
    def param_gap(a): a[0]["paths"]["/v1/proofs/{round}/{shard}/{index}"]["get"]["parameters"].pop(0)
    def dup_id(a): a[0]["paths"]["/v1/served/{metric}"]["get"]["operationId"] = "getHealth"
    for name, f in list(locals().items()):
        if callable(f) and name not in ("plant",):
            plant(name, f)
    if faults:
        raise SystemExit(f"check-openapi self-test: planted faults not caught: {faults}")
    print("openapi self-test ok: 12 planted faults fail")


if __name__ == "__main__":
    if sys.argv[1:] == ["--self-test"]:
        self_test()
        sys.exit(0)
    spec, *rest = inputs()
    problems = check(spec, *rest)
    agent = os.path.join(ROOT, "apps/agent/openapi.json")
    if not os.path.exists(agent) or open(agent, encoding="utf-8").read() != rest[-1]:
        problems.append("apps/agent/openapi.json differs from docs/api/openapi.json (node apps/agent/scripts/sync.mjs)")
    for b in problems:
        print(f"openapi: {b}")
    if problems:
        print(f"openapi: {len(problems)} problem(s)")
        sys.exit(1)
    n = sum(1 for item in spec["paths"].values() for m in item if m in METHODS)
    print(f"openapi ok: {n} operations match the code, {len(spec['x-onyx-error-codes'])} error codes, Caddy, the site and agent copies")
