#!/usr/bin/env python3
# WCAG 2 contrast ratios of the site palette (docs/website-plan.md W1/W3): every text colour
# in src/lib/app.css on every background it is drawn on (translucent ones composited over
# --bg). Prints a Markdown table, lowest first; flags < 4.5:1 (body text, AA).
# Usage: python3 -I apps/web/tests/contrast.py [path/to/app.css]
import re, sys
from pathlib import Path

css = Path(sys.argv[1] if len(sys.argv) > 1 else Path(__file__).parent.parent / "src/lib/app.css").read_text()
root = css[css.index(":root {"):]
root = root[: root.index("}")]
var = {m[1]: m[2].strip() for m in re.finditer(r"(--[\w-]+):\s*([^;]+);", root)}


def rgba(v):
    v = var.get(v, v)
    if v.startswith("#"):
        return tuple(int(v[i:i + 2], 16) for i in (1, 3, 5)), 1.0
    m = re.fullmatch(r"rgb\((\d+) (\d+) (\d+)(?: / ([\d.]+))?\)", v)
    if not m:
        raise SystemExit(f"cannot read colour {v!r}")
    return tuple(int(m[i]) for i in (1, 2, 3)), float(m[4] or 1)


def over(c, bg):
    (rgb, a), (b, _) = c, bg
    return tuple(round(rgb[i] * a + b[i] * (1 - a)) for i in range(3)), 1.0


def lum(rgb):
    f = lambda v: v / 12.92 if v <= 0.03928 else ((v + 0.055) / 1.055) ** 2.4
    r, g, b = (f(x / 255) for x in rgb)
    return 0.2126 * r + 0.7152 * g + 0.0722 * b


def ratio(a, b):
    hi, lo = sorted((lum(a), lum(b)), reverse=True)
    return (hi + 0.05) / (lo + 0.05)


bg = rgba("--bg")
# Backgrounds text sits on; the header's is set in +layout.svelte.
# W10: the header, the slab's glass note and hovered rows are tokens too.
BACKS = {n: over(rgba(n), bg) for n in ("--bg", "--bg-2", "--panel", "--panel-solid", "--slab", "--btc-soft", "--header-bg", "--slab-glass", "--good-soft")}
BACKS["--hover on --panel-solid"] = over(rgba("--hover"), over(rgba("--panel-solid"), bg))
TEXT = ["--ink", "--muted", "--accent", "--accent-2", "--btc", "--good", "--bad", "--warn", "--line-net", "--slab-ink", "--slab-muted"]
# Chart fills (non-text, WCAG 1.4.11 asks 3:1).
FILLS = ["--pool-1", "--pool-2", "--pool-3", "--pool-4", "--pool-5", "--pool-6", "--pool-other"]
rows = [(ratio(rgba(t)[0], b[0]), t, var[t], n, "text", 4.5) for t in TEXT for n, b in BACKS.items()]
rows.append((ratio(rgba("--accent-ink")[0], rgba("--accent")[0]), "--accent-ink", var["--accent-ink"], "--accent (filled button, pressed chip)", "text", 4.5))
rows += [(ratio(rgba(t)[0], b[0]), t, var[t], n, "fill", 3.0) for t in FILLS for n, b in BACKS.items() if n in ("--bg", "--panel", "--panel-solid")]
print("| Colour | Value | On | Kind | Ratio | AA |")
print("|---|---|---|---|---|---|")
for r, t, v, n, k, need in sorted(rows):
    print(f"| {t} | {v} | {n} | {k} | {r:.2f}:1 | {'FAIL' if r < need else 'pass'} (needs {need}:1) |")
fails = sum(1 for r, *_, need in rows if r < need)
print(f"\n{len(rows)} pairs, {fails} below AA")
