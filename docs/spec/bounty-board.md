# Spec: the bounty board (draft, NORTH-STAR NS15, north-star loop NL9, ROADMAP S10)

Status: **Slice 1 built 2026-10-28** (ADR-0051; the identity is a signed want text from an open
prepaid deposit instead of a voucher header, see the ADR item 2). Drafted 2026-10-27 by lane B. Prior art:
RESEARCH.md "Lane B batch 2 (2026-10-27)".

## Scope

Contributors want to know what data buyers want; buyers must not reveal what exactly they look
for (an exact query can be trading intent or a person's interest). The board shows **coarse
categories of wanted data, never exact queries**:

- a want is a tuple from **closed lists only**: `kind` (the six kinds of the submit flow: chain
  data, public page or API, keyed API, signed by the source, own data, forecast), `domain` (a
  fixed list: finance, energy, weather and climate, earth science, health statistics, transport,
  public spending, other), `region` (continent or `global`, never a city or coordinates), and
  `cadence` (once, daily, monthly);
- **no free text, no URL, no metric id, no subject, no date**;
- the board shows each category's demand as a **bucket** (`1-4`, `5-19`, `20+`), and a category is
  shown only once it has wants from at least **K** distinct prepaid accounts (k-anonymity: one
  buyer is never alone in a bucket);
- counts move once a week, so the time of one want cannot be read off the board.

Optional money (later, needs NL8's hold): a want may pledge a credit that the first paid query of
a matching observed fact uses up. Version 1 has no money.

## Interfaces

- **`crates/onyx-intake/src/bounty.rs`** (new, no I/O, next to the submit questions):
  `Want { kind, domain, region, cadence }` with closed enums, serde that refuses unknown fields and
  values, `Board::add(account_hash, want)`, `Board::published(k)` returning the buckets.
- **`crates/onyx-api`**: `POST /v1/wants` (a prepaid voucher header proves an account without
  charging it; one want per category per account; the account is stored only as a salted hash),
  `GET /v1/wants` (the published buckets, cached a week). OpenAPI file and check updated.
- **Site:** a "Wanted data" panel on the submit page (`apps/web/src/routes/submit/`) listing the
  shown categories and their buckets, linking each to the submit flow with `kind` preset.

## Rules

1. **R-BNT-1.** A want that carries any field outside the four closed lists is refused (400
   `bounty_too_specific`), so an exact query cannot be posted at all.
2. **R-BNT-2.** A category with wants from fewer than K accounts is not shown; counts are
   bucketed; the board's numbers change only at the weekly cut.
3. **R-BNT-3.** No route returns which account posted which want; the account hash is salted
   with a node secret and never served.
4. **R-BNT-4.** Labels rule: a want can never name an entity or a person (there is no field for
   it).

## Tests that prove the Done-when

- `onyx-intake` `bounty::tests::a_want_with_free_text_or_a_subject_is_refused` (the "test refuses
  a bounty that names an exact query or subject"), `::unknown_enum_values_are_refused`,
  `::a_category_below_k_is_not_published`, `::counts_are_bucketed`.
- `onyx-api` `tests::wants_route_publishes_buckets_only`, `tests::one_account_one_want_per_category`.
- e2e "the wanted-data panel shows categories, never queries".

## Assumptions

- Prepaid accounts (ADR-0046) are the only identity; making many accounts costs a deposit each,
  which bounds how cheaply one buyer can pass K alone.
- Eight domains and six continents keep the cells large enough to say nothing about one buyer.

## Open questions

1. K: 3 or 5? Larger hides better and shows less.
2. Does a want expire (30 days) so the board shows current demand?
3. Should contributors see buckets per `kind` only (coarser) until the board has enough wants?
