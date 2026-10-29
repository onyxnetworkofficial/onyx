# ADR-0040: Bounded contributor data payloads

## Status

Accepted for the local intake workflow (U3). This decision does not authorize public hosting,
publication, redistribution, private-data access control, or a production deployment.

## Context

The contributor flow accepts metadata about sources, but the product needs a low-friction path for a
person to propose a small dataset they own and have permission to share. A file must not become a
public upload merely because it was submitted, and file parsing must not execute content.

## Decision

1. Add an optional versioned payload object to a submission: format (`csv` or `json`), UTF-8
   content, exact byte count, and Keccak-256 of the exact UTF-8 content. The payload cap is 8 KiB.
   The enclosing JSON request cap is 64 KiB to account for JSON escaping.
2. Payloads are accepted only when answers identify `own` and `own_data`. A missing source URL is
   accepted only on that same route. Supplying a source URL still runs the existing HTTPS and
   secret-parameter checks. Other submission routes retain the required URL.
3. The browser reads text only, validates and previews it before send, and never evaluates it.
   Reject malformed JSON/CSV, duplicate JSON object keys, spreadsheet-formula-like cells, HTML or
   executable markup, common credential patterns, and personal-contact/person fields detected by
   the guard. These checks are conservative signals, not proof that content is safe; a reviewer
   still checks rights and content before approval.
4. The same canonical checks run in Rust before journaling. The digest is recomputed from content;
   claimed byte counts and hashes must match. The raw payload is stored only in the existing
   operator-private append-only review journal. It is not exposed by receipt/status or a new public
   read route. No arbitrary public upload directory is introduced.
5. Metadata-only submissions remain available, including on the own-data route. Offline builds
   continue to download a JSON proposal locally; connected submissions send the same object to the
   guarded node intake. Metadata-only submission hashes retain the v1 Borsh layout for receipt and
   journal compatibility; submissions with a payload use a v2 domain and Borsh layout that commits
   to the payload.

## Consequences

- An 8 KiB payload is useful for a small example or proposal, not bulk dataset hosting.
- Users must affirm they have permission to share it. Automated checks cannot determine legal
  rights or identify every person or credential; approval remains a human review decision.
- The current local journal is not a multi-tenant access-controlled storage service. Public/private
  dataset access, retention, deletion, and larger payloads require separate decisions.
- Browser and Rust validators need parity tests for every accepted format and refusal rule.

## Verification

`onyx-intake` unit tests and shared web tests cover bounds, digest mismatch, malformed and duplicate
JSON keys, CSV structure, formula/markup/credential/contact guards, URL-free own data, and refusing
payloads on other routes. API tests cover request bounds and prove that public status contains only
the safe receipt fields. Browser tests exercise a real local CSV through preview, download, and the
connected test node.
