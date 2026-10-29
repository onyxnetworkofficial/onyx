// Build mode (AUD-2-F1). `ONYX_PUBLIC=1 scripts/build-web.sh` sets
// VITE_ONYX_PUBLIC=1: the build ships no label rows (the test labels' sharing
// rights are not cleared), so the checks that rerun the recipe over the label
// set check the published rows against the pinned roots instead. Every other
// build is the private preview and says so in a banner.
export const PUBLIC = import.meta.env.VITE_ONYX_PUBLIC === '1';
// Rights fallback (ROADMAP L14, ADR-0035, THREATS L8-5). `ONYX_RIGHTS=fallback
// ONYX_PUBLIC=1 scripts/build-web.sh` sets VITE_ONYX_RIGHTS=fallback: the build
// ships no total derived from the entity label sets (company flows, balances,
// reserves, per-entity history) and says why where they would be. Vite replaces
// the variable with a literal, so code and figures behind `FALLBACK ? … : …` are
// left out of the bundle; the public-build guard checks that they are.
export const FALLBACK = import.meta.env.VITE_ONYX_RIGHTS === 'fallback';
