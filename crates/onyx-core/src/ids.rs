//! Identifier grammars. Every protocol identifier is plain ASCII with a fixed
//! grammar, so there is one spelling per id: no case variants, homoglyphs,
//! whitespace or free-text names (THREATS R-LBL-CANON-1, R-PRIV-1).
//!
//! Address bodies are case-preserving here because some encodings are
//! case-sensitive (base58). Each namespace's canonical form is checked by an
//! [`AddressCanon`] that the sector adapter provides.

/// Maximum length of a dotted id (metric, source, chain, label set, terms, kind).
pub const MAX_DOTTED_LEN: usize = 128;
/// Maximum length of an entity slug after `entity:`.
pub const MAX_ENTITY_SLUG_LEN: usize = 63;
/// Maximum length of an address or instrument body.
pub const MAX_BODY_LEN: usize = 128;
/// Maximum length of a namespace prefix.
pub const MAX_NAMESPACE_LEN: usize = 16;

/// Lowercase dotted id: segments of `[a-z0-9_-]` joined by single `.` or `:`,
/// starting and ending with `[a-z0-9]`, at most [`MAX_DOTTED_LEN`]. No leading,
/// trailing or doubled separators, so `chain.x` and `chain.x.` cannot both exist
/// (R-ID-2). Used for metric, source, chain, label set, aggregate, terms and kind ids.
pub fn is_dotted_id(s: &str) -> bool {
    let b = s.as_bytes();
    let alnum = |c: &u8| c.is_ascii_lowercase() || c.is_ascii_digit();
    let sep = |c: &u8| matches!(c, b'.' | b':');
    !b.is_empty()
        && b.len() <= MAX_DOTTED_LEN
        && b.first().is_some_and(alnum)
        && b.last().is_some_and(alnum)
        && b.iter()
            .all(|c| alnum(c) || sep(c) || matches!(c, b'_' | b'-'))
        && b.windows(2)
            .all(|w| !((sep(&w[0]) || is_dash(w[0])) && (sep(&w[1]) || is_dash(w[1]))))
}

fn is_dash(c: u8) -> bool {
    matches!(c, b'-' | b'_')
}

/// Entity id: `entity:<slug>`, slug of `[a-z0-9]` words joined by single `-`,
/// at most [`MAX_ENTITY_SLUG_LEN`].
///
/// The grammar alone cannot tell an organisation from a person
/// (`entity:jane-doe` is well formed), nor `c0inbase` from `coinbase`. The
/// [`crate::Registry`] closes both: only registered entities are accepted.
pub fn is_entity_id(s: &str) -> bool {
    let Some(slug) = s.strip_prefix("entity:") else {
        return false;
    };
    let b = slug.as_bytes();
    !b.is_empty()
        && b.len() <= MAX_ENTITY_SLUG_LEN
        && b.split(|c| *c == b'-').all(|w| {
            !w.is_empty()
                && w.iter()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        })
}

fn split_namespaced(s: &str) -> Option<(&str, &str)> {
    let (ns, body) = s.split_once(':')?;
    let ns_ok = !ns.is_empty()
        && ns.len() <= MAX_NAMESPACE_LEN
        && ns
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    let body_ok = !body.is_empty() && body.len() <= MAX_BODY_LEN;
    (ns_ok && body_ok).then_some((ns, body))
}

/// Address: `<ns>:<body>`, ns `[a-z0-9]+`, body `[A-Za-z0-9]+`.
/// Returns the namespace and body when well formed.
pub fn parse_address(s: &str) -> Option<(&str, &str)> {
    let (ns, body) = split_namespaced(s)?;
    body.bytes()
        .all(|c| c.is_ascii_alphanumeric())
        .then_some((ns, body))
}

/// Instrument: `<ns>:<body>`, body is uppercase `[A-Z0-9]` words joined by a
/// single `.` or `-` (e.g. `ticker:IBIT`, `ticker:BRK.B`, `series:FEDFUNDS`), at
/// most 32 bytes. One spelling per instrument (R-SUBJ-2).
pub fn is_instrument_id(s: &str) -> bool {
    split_namespaced(s).is_some_and(|(_, body)| {
        body.len() <= 32
            && body
                .as_bytes()
                .split(|c| matches!(c, b'.' | b'-'))
                .all(|w| {
                    !w.is_empty()
                        && w.iter()
                            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
                })
    })
}

/// License id: visible ASCII (0x21..=0x7E), 1..=64 bytes. SPDX ids fit; zero-width
/// and other invisible characters do not.
pub fn is_license_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.bytes().all(|c| c.is_ascii_graphic())
}

/// Canonical-form check for one address namespace, supplied by a sector adapter.
/// `Any` lets settlement check that a registry runs the built-in type for a
/// rule id, not caller-supplied code with the same id ([`crate::RuleSet`]).
pub trait AddressCanon: std::any::Any {
    /// Namespace this canon covers, e.g. `btc`.
    fn namespace(&self) -> &str;
    /// Versioned id of the rule itself (dotted id), e.g. `onyx.canon.btc-mainnet.v1`.
    /// Part of the registry root, so a changed rule changes the root (R-REG-1).
    fn canon_id(&self) -> &str;
    /// Fixed `(body, expected is_canonical)` vectors. The registry root hashes the
    /// results of running them, so changing the rule's behaviour without bumping
    /// `canon_id` still changes the root (R-REG-3).
    fn vectors(&self) -> &'static [(&'static str, bool)];
    /// True if `body` is the single canonical spelling of an address in this namespace.
    fn is_canonical(&self, body: &str) -> bool;
}

/// Result of checking a proof that a signer controls an address.
#[derive(Clone, Copy, Debug, PartialEq, Eq, borsh::BorshSerialize)]
pub enum ProofOutcome {
    /// The signature proves control of the address for the message.
    Valid,
    /// The signature is wrong, malformed, non-canonical, or outside Onyx policy.
    Invalid,
    /// The verifier cannot interpret the address's script. Never counts as proof.
    Inconclusive,
}

/// One fixed test vector for an [`AddressProof`].
#[derive(Clone, Copy, Debug)]
pub struct ProofVector {
    /// Address body (without namespace).
    pub body: &'static str,
    /// Signed message.
    pub message: &'static str,
    /// Encoded signature.
    pub signature: &'static str,
    /// Expected outcome.
    pub expect: ProofOutcome,
}

/// Rule for checking that the holder of an address signed a message (ADR-0015).
/// Sector adapters provide it per namespace, as with [`AddressCanon`]: core
/// crates never name a chain's signature scheme. `Any`: see [`AddressCanon`].
pub trait AddressProof: std::any::Any {
    /// Namespace this verifier covers, e.g. `btc`.
    fn namespace(&self) -> &str;
    /// Chain id whose snapshot heights judge this namespace's attestations,
    /// e.g. `btc.mainnet` (matches `Snapshot::BlockRange::chain`).
    fn chain(&self) -> &str;
    /// Versioned id of the rule (dotted id), e.g. `onyx.proof.btc.bip322.v1`.
    /// Part of the registry root.
    fn proof_id(&self) -> &str;
    /// Longest allowed attestation validity window, in this chain's blocks.
    fn max_window(&self) -> u64;
    /// Fixed vectors; the registry root hashes their results (as for canons).
    fn vectors(&self) -> &'static [ProofVector];
    /// Check `signature` over `message` for the address `body`. Must be a pure
    /// function of its inputs, so every validator gets the same outcome.
    fn verify(&self, body: &str, message: &[u8], signature: &str) -> ProofOutcome;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dotted_ids() {
        assert!(is_dotted_id("crypto.btc.entity_balance"));
        assert!(is_dotted_id("eip155:8453"));
        for bad in [
            "",
            "Crypto",
            "a b",
            "jane doe",
            ".x",
            "x.",
            "x..y",
            "x.:y",
            "x-.y",
            "x\u{200B}",
            "ünï",
        ] {
            assert!(!is_dotted_id(bad), "{bad:?}");
        }
    }

    #[test]
    fn entity_ids() {
        assert!(is_entity_id("entity:coinbase"));
        assert!(is_entity_id("entity:21shares"));
        for bad in [
            "coinbase",
            "person:jane-doe",
            "entity:",
            "entity:Coinbase",
            "entity:c\u{043E}inbase", // Cyrillic o
            "entity:jane doe",
            "Jane Doe (home wallet)",
            "entity:coinbase-",
            "entity:-coinbase",
            "entity:coinbase--x",
        ] {
            assert!(!is_entity_id(bad), "{bad:?}");
        }
    }

    #[test]
    fn addresses_and_instruments() {
        assert_eq!(parse_address("btc:bc1qxyz"), Some(("btc", "bc1qxyz")));
        assert!(parse_address("btc:bc1q xyz").is_none());
        assert!(parse_address("BTC:bc1q").is_none());
        assert!(parse_address("btc:").is_none());
        assert!(is_instrument_id("ticker:IBIT"));
        assert!(is_instrument_id("ticker:BRK.B"));
        assert!(is_instrument_id("series:FEDFUNDS"));
        for bad in [
            "ticker:IB IT",
            "ticker:ibit",
            "ticker:IBIT.",
            "ticker:IBIT-",
            "ticker:.IBIT",
            "ticker:IB..IT",
            "ticker:---",
            "ticker:JANE.DOE.12-MAIN-ST-SPRINGFIELD-USA",
        ] {
            assert!(!is_instrument_id(bad), "{bad}");
        }
    }

    #[test]
    fn license_ids() {
        assert!(is_license_id("CC-BY-4.0"));
        assert!(is_license_id("onyx:public-chain-data"));
        assert!(!is_license_id("\u{200B}"));
        assert!(!is_license_id(" "));
        assert!(!is_license_id(""));
    }
}
