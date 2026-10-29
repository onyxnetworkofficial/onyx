//! First-party attestations (ADR-0015).
//!
//! An attestation says "this address belongs to this entity" and carries two
//! signatures over the same statement text:
//! 1. the address signature (for example BIP-322), which proves the signer
//!    controls the address, checked by the namespace's [`AddressProof`];
//! 2. the entity signature (EIP-191 `personal_sign`), by a key the registry
//!    lists for that entity, which proves the entity claims the address.
//!
//! Signature 1 alone proves key control only: anyone could sign "my address
//! belongs to entity:okx" for their own address and inflate OKX's numbers.
//! Signature 2 alone proves nothing about the address. Both are required.

use borsh::{BorshDeserialize, BorshSerialize};
use onyx_core::ids::{is_dotted_id, is_entity_id, parse_address};
use onyx_core::registry::is_registered_canonical_address;
use onyx_core::{
    keccak256, to_canonical_bytes, AddressProof, Hash32, KeyState, ProofOutcome, Registry,
};
use secp256k1::ecdsa::{RecoverableSignature, RecoveryId};
use secp256k1::{Message, Secp256k1};

/// First line of every statement. The version is part of the signed text.
pub const STATEMENT_HEADER: &str = "Onyx address attestation v1";
/// Nonce length in bytes (32 lowercase hex characters in the text).
pub const NONCE_LEN: usize = 16;
/// Longest statement text accepted, in bytes. Every field is bounded by its grammar.
pub const MAX_STATEMENT_LEN: usize = 1024;
/// Longest encoded address signature accepted, in bytes (DoS bound on decoding).
pub const MAX_ADDRESS_SIG_LEN: usize = 8192;

/// Parsed attestation statement. The signed bytes are exactly [`Statement::render`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Statement {
    /// Network the statement is for (dotted id), e.g. `onyx.devnet`. Stops replay across networks.
    pub domain: String,
    /// Registered entity id, e.g. `entity:okx`.
    pub entity: String,
    /// Namespaced canonical address, e.g. `btc:bc1q...`.
    pub address: String,
    /// First block height (in the address's chain) at which the statement holds.
    pub valid_from: u64,
    /// First block height at which it no longer holds.
    pub expires: u64,
    /// Random nonce chosen by the signer.
    pub nonce: [u8; NONCE_LEN],
}

impl Statement {
    /// The exact text both parties sign: `\n`-separated lines, no trailing newline.
    pub fn render(&self) -> String {
        format!(
            "{STATEMENT_HEADER}\ndomain: {}\nentity: {}\naddress: {}\nvalid_from: {}\nexpires: {}\nnonce: {}",
            self.domain,
            self.entity,
            self.address,
            self.valid_from,
            self.expires,
            hex::encode(self.nonce)
        )
    }

    /// Strict parse. Every field must follow its grammar (none can hold a
    /// newline), and the text must be exactly what [`Statement::render`] gives,
    /// so one statement has one spelling (R-ATTEST-1).
    pub fn parse(text: &str) -> Result<Self, AttestError> {
        if text.len() > MAX_STATEMENT_LEN {
            return Err(AttestError::Malformed);
        }
        let mut lines = text.split('\n');
        if lines.next() != Some(STATEMENT_HEADER) {
            return Err(AttestError::Malformed);
        }
        let mut field = |name: &str| -> Result<&str, AttestError> {
            lines
                .next()
                .and_then(|l| l.strip_prefix(name))
                .and_then(|l| l.strip_prefix(": "))
                .ok_or(AttestError::Malformed)
        };
        let domain = field("domain")?.to_string();
        let entity = field("entity")?.to_string();
        let address = field("address")?.to_string();
        let valid_from = parse_height(field("valid_from")?)?;
        let expires = parse_height(field("expires")?)?;
        let nonce_hex = field("nonce")?;
        if lines.next().is_some() {
            return Err(AttestError::Malformed);
        }
        let nonce = parse_nonce(nonce_hex)?;
        let s = Statement {
            domain,
            entity,
            address,
            valid_from,
            expires,
            nonce,
        };
        s.check_grammar()?;
        if s.render() != text {
            return Err(AttestError::Malformed);
        }
        Ok(s)
    }

    /// Field grammars, independent of any registry.
    pub fn check_grammar(&self) -> Result<(), AttestError> {
        if !is_dotted_id(&self.domain)
            || !is_entity_id(&self.entity)
            || parse_address(&self.address).is_none()
        {
            return Err(AttestError::Malformed);
        }
        if self.valid_from >= self.expires {
            return Err(AttestError::BadWindow);
        }
        Ok(())
    }
}

/// Decimal height, no sign, no leading zeros (except `0` itself).
fn parse_height(s: &str) -> Result<u64, AttestError> {
    let ok = !s.is_empty()
        && s.len() <= 20
        && s.bytes().all(|c| c.is_ascii_digit())
        && (s == "0" || !s.starts_with('0'));
    if !ok {
        return Err(AttestError::Malformed);
    }
    s.parse().map_err(|_| AttestError::Malformed)
}

/// Exactly `2 * NONCE_LEN` lowercase hex characters.
fn parse_nonce(s: &str) -> Result<[u8; NONCE_LEN], AttestError> {
    if s.len() != 2 * NONCE_LEN || !s.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f')) {
        return Err(AttestError::Malformed);
    }
    let mut out = [0u8; NONCE_LEN];
    hex::decode_to_slice(s, &mut out).map_err(|_| AttestError::Malformed)?;
    Ok(out)
}

/// A signed attestation, as stored and referenced from label evidence.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Attestation {
    /// Statement text, exactly as signed.
    pub statement: String,
    /// Address signature in the namespace's encoding (BTC: BIP-322 `smp`/`ful`).
    pub address_sig: String,
    /// Entity key (20-byte EVM-style address) that signed `statement`.
    pub entity_key: [u8; 20],
    /// EIP-191 signature `r ‖ s ‖ v`, `v` in {27, 28}, low `s`.
    pub entity_sig: [u8; 65],
}

/// Over which block ranges an attestation is judged.
///
/// For a shard it is derived from the shard's own snapshot by
/// `onyx_recipe_host::assemble_input`, never chosen by the caller (L13-F3).
/// The network domain comes from the registry, whose root the shard pins.
#[derive(Clone, Copy, Debug)]
pub struct AttestContext<'a> {
    /// Per address namespace: the inclusive block range `(from, to)` the data
    /// covers, e.g. `[("btc", 921_080, 921_082)]`. An attestation must hold over
    /// the whole range; a namespace without a range cannot be judged.
    pub ranges: &'a [(&'a str, u64, u64)],
}

impl AttestContext<'_> {
    /// Judging range for an address namespace.
    pub fn range(&self, namespace: &str) -> Option<(u64, u64)> {
        self.ranges
            .iter()
            .find(|(ns, _, _)| *ns == namespace)
            .map(|(_, from, to)| (*from, *to))
    }
}

/// Attestation error. Every variant means "no valid attestation"; none is a
/// statement about the address belonging to anyone else.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AttestError {
    /// Statement text malformed or not in its single canonical spelling.
    #[error("statement malformed")]
    Malformed,
    /// `valid_from >= expires`, or the window is longer than the namespace allows.
    #[error("validity window empty or too long")]
    BadWindow,
    /// Statement is for another network, or the registry has no domain.
    #[error("statement domain does not match this network")]
    WrongDomain,
    /// Entity not registered.
    #[error("entity not registered")]
    UnknownEntity,
    /// Address not canonical, or its namespace has no canon.
    #[error("address not canonical")]
    AddressNotCanonical,
    /// No address-proof rule for the namespace.
    #[error("no proof rule for namespace")]
    NoProofRule,
    /// No judging range for the address's namespace in the context, or an
    /// inverted one (`from > to`, L14-F7).
    #[error("no judging range for the address namespace")]
    NoRange,
    /// The entity revoked this address at or before the end of the range.
    #[error("attestation revoked")]
    Revoked,
    /// Range starts before `valid_from`.
    #[error("attestation not yet valid")]
    NotYetValid,
    /// Range ends at or after `expires`.
    #[error("attestation expired")]
    Expired,
    /// Statement or address signature over the size limits.
    #[error("attestation too large")]
    TooLarge,
    /// Address signature invalid.
    #[error("address signature invalid")]
    AddressSigInvalid,
    /// Verifier cannot interpret the address script; not counted as proof.
    #[error("address signature inconclusive")]
    AddressSigInconclusive,
    /// Entity key not registered for this entity.
    #[error("entity key not registered for entity")]
    EntityKeyUnknown,
    /// Entity signature invalid, non-canonical, or from another key.
    #[error("entity signature invalid")]
    EntitySigInvalid,
}

impl Attestation {
    /// Content hash of the canonical encoding. Label evidence cites this.
    pub fn content_hash(&self) -> Hash32 {
        let mut b = b"onyx-attestation-v1".to_vec();
        b.extend_from_slice(&to_canonical_bytes(self).unwrap_or_default());
        keccak256(&b)
    }

    /// Full check against the registry over `ctx`'s ranges. Returns the parsed statement.
    /// Pure: same inputs and registry give the same answer on every validator.
    pub fn verify(
        &self,
        ctx: &AttestContext<'_>,
        reg: &dyn Registry,
    ) -> Result<Statement, AttestError> {
        self.verify_with(ctx, reg, |s| s == Some(KeyState::Active))
    }

    /// As [`Attestation::verify`], but as history: a key the entity has since
    /// retired still counts (not a compromised one). Used to build scoring
    /// resolutions, so routine key rotation cannot move a resolution's round
    /// (L15-F1).
    pub fn verify_history(
        &self,
        ctx: &AttestContext<'_>,
        reg: &dyn Registry,
        finalized_round: u64,
    ) -> Result<Statement, AttestError> {
        // A retired key counts only for attestations finalized before it was
        // retired, so it cannot create new history (L16-F5).
        self.verify_with(ctx, reg, |s| match s {
            Some(KeyState::Active) => true,
            Some(KeyState::Retired { at_round }) => finalized_round < at_round,
            _ => false,
        })
    }

    fn verify_with(
        &self,
        ctx: &AttestContext<'_>,
        reg: &dyn Registry,
        key_ok: impl Fn(Option<KeyState>) -> bool,
    ) -> Result<Statement, AttestError> {
        let st = self.verify_static(reg, key_ok)?;
        check_range(&st, ctx, reg)?;
        Ok(st)
    }

    /// Everything that does not depend on the block range: statement, domain,
    /// entity, address, window length, key state and both signatures. Its result
    /// depends only on the attestation and the registry, so [`AttestCache`] can
    /// keep it per (attestation, registry root) across shards (L15-F4).
    fn verify_static(
        &self,
        reg: &dyn Registry,
        key_ok: impl Fn(Option<KeyState>) -> bool,
    ) -> Result<Statement, AttestError> {
        // Size bounds before any parsing or hashing (L12-F5).
        if self.address_sig.len() > MAX_ADDRESS_SIG_LEN {
            return Err(AttestError::AddressSigInvalid);
        }
        let st = Statement::parse(&self.statement)?;
        if reg.domain().is_empty() || st.domain != reg.domain() {
            return Err(AttestError::WrongDomain);
        }
        if reg.entity_kind(&st.entity).is_none() {
            return Err(AttestError::UnknownEntity);
        }
        if !is_registered_canonical_address(&st.address, reg) {
            return Err(AttestError::AddressNotCanonical);
        }
        let (ns, body) = parse_address(&st.address).ok_or(AttestError::Malformed)?;
        let proof: &dyn AddressProof = reg.address_proof(ns).ok_or(AttestError::NoProofRule)?;
        if st.expires - st.valid_from > proof.max_window() {
            return Err(AttestError::BadWindow);
        }
        // Cheap checks first; the address proof is the expensive one.
        if !key_ok(reg.entity_key_state(&st.entity, &self.entity_key)) {
            return Err(AttestError::EntityKeyUnknown);
        }
        if eip191_recover(self.statement.as_bytes(), &self.entity_sig) != Some(self.entity_key) {
            return Err(AttestError::EntitySigInvalid);
        }
        match proof.verify(body, self.statement.as_bytes(), &self.address_sig) {
            ProofOutcome::Valid => Ok(st),
            ProofOutcome::Invalid => Err(AttestError::AddressSigInvalid),
            ProofOutcome::Inconclusive => Err(AttestError::AddressSigInconclusive),
        }
    }
}

/// The range-dependent part of verification: the statement must hold over the
/// whole range the data covers (L13-F3) and not be revoked within it.
pub fn check_range(
    st: &Statement,
    ctx: &AttestContext<'_>,
    reg: &dyn Registry,
) -> Result<(), AttestError> {
    let (ns, _) = parse_address(&st.address).ok_or(AttestError::Malformed)?;
    let (from, to) = ctx
        .range(ns)
        .filter(|(from, to)| from <= to)
        .ok_or(AttestError::NoRange)?;
    if from < st.valid_from {
        return Err(AttestError::NotYetValid);
    }
    if to >= st.expires {
        return Err(AttestError::Expired);
    }
    if reg
        .revoked_from(&st.entity, &st.address)
        .is_some_and(|revoked| revoked <= to)
    {
        return Err(AttestError::Revoked);
    }
    Ok(())
}

/// Verdict cache for the range-independent checks, keyed by (attestation
/// content hash, registry root). One validator checks each attestation's
/// signatures once per registry, not once per shard (L15-F4).
#[derive(Default)]
pub struct AttestCache {
    map: std::sync::Mutex<
        std::collections::BTreeMap<(Hash32, Hash32), Result<Statement, AttestError>>,
    >,
}

impl AttestCache {
    /// Empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of cached verdicts.
    pub fn len(&self) -> usize {
        self.map.lock().map(|m| m.len()).unwrap_or(0)
    }

    /// True if nothing is cached.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// As [`Attestation::verify`], with the static part cached under the
    /// attestation's own content hash (computed here, so a caller cannot file a
    /// verdict under another attestation's hash, L16-F8) and `registry_root`
    /// (the caller passes `reg.root()`, computed once per call site).
    pub fn verify(
        &self,
        att: &Attestation,
        registry_root: Hash32,
        ctx: &AttestContext<'_>,
        reg: &dyn Registry,
    ) -> Result<Statement, AttestError> {
        self.verify_hashed(att, att.content_hash(), registry_root, ctx, reg)
    }

    /// As [`AttestCache::verify`] with a hash the crate already computed from
    /// the same attestation.
    pub(crate) fn verify_hashed(
        &self,
        att: &Attestation,
        hash: Hash32,
        registry_root: Hash32,
        ctx: &AttestContext<'_>,
        reg: &dyn Registry,
    ) -> Result<Statement, AttestError> {
        let key = (hash, registry_root);
        let cached = self.map.lock().ok().and_then(|m| m.get(&key).cloned());
        let st = match cached {
            Some(v) => v,
            None => {
                let v = att.verify_static(reg, |s| s == Some(KeyState::Active));
                if let Ok(mut m) = self.map.lock() {
                    m.insert(key, v.clone());
                }
                v
            }
        }?;
        check_range(&st, ctx, reg)?;
        Ok(st)
    }
}

/// EIP-191 version 0x45 digest: `keccak256("\x19Ethereum Signed Message:\n" ‖ len ‖ msg)`,
/// `len` in ASCII decimal.
pub fn eip191_digest(msg: &[u8]) -> Hash32 {
    let mut b = format!("\x19Ethereum Signed Message:\n{}", msg.len()).into_bytes();
    b.extend_from_slice(msg);
    keccak256(&b)
}

/// Recover the 20-byte signer of an EIP-191 signature. Only the canonical form
/// is accepted: `v` in {27, 28} and `s <= n/2` (EIP-2), so one signer and
/// message have exactly one accepted signature per nonce `k`.
pub fn eip191_recover(msg: &[u8], sig: &[u8; 65]) -> Option<[u8; 20]> {
    let recid = match sig[64] {
        27 => RecoveryId::from_i32(0).ok()?,
        28 => RecoveryId::from_i32(1).ok()?,
        _ => return None,
    };
    let rsig = RecoverableSignature::from_compact(&sig[..64], recid).ok()?;
    let std = rsig.to_standard();
    let mut low = std;
    low.normalize_s();
    if low != std {
        return None;
    }
    let digest = Message::from_digest(eip191_digest(msg).0);
    let pk = Secp256k1::verification_only()
        .recover_ecdsa(&digest, &rsig)
        .ok()?;
    let uncompressed = pk.serialize_uncompressed();
    let h = keccak256(&uncompressed[1..]);
    let mut out = [0u8; 20];
    out.copy_from_slice(&h.0[12..]);
    Some(out)
}

/// EIP-191 sign `msg` with the 32-byte secret key `secret`, in the one form
/// [`eip191_recover`] accepts (`v` in {27, 28}; libsecp256k1 always gives a
/// low `s`). `None` when `secret` is not a valid secp256k1 scalar.
pub fn eip191_sign_with(secret: &[u8; 32], msg: &[u8]) -> Option<[u8; 65]> {
    let sk = secp256k1::SecretKey::from_slice(secret).ok()?;
    let digest = Message::from_digest(eip191_digest(msg).0);
    let (recid, rs) = Secp256k1::signing_only()
        .sign_ecdsa_recoverable(&digest, &sk)
        .serialize_compact();
    let mut out = [0u8; 65];
    out[..64].copy_from_slice(&rs);
    out[64] = 27 + u8::try_from(recid.to_i32()).ok()?;
    Some(out)
}

/// The 20-byte EVM address of a 32-byte secret key (`None` when it is not a
/// valid secp256k1 scalar).
pub fn secret_address(secret: &[u8; 32]) -> Option<[u8; 20]> {
    let sk = secp256k1::SecretKey::from_slice(secret).ok()?;
    let pk = sk
        .public_key(&Secp256k1::signing_only())
        .serialize_uncompressed();
    let mut out = [0u8; 20];
    out.copy_from_slice(&keccak256(&pk[1..]).0[12..]);
    Some(out)
}

#[cfg(test)]
pub(crate) mod test_support {
    //! Test signer and a stub address proof. The key is derived from a fixed
    //! label; it is not a real key and holds nothing on any network.
    use super::*;
    use onyx_core::ProofVector;
    use secp256k1::SecretKey;

    /// Test entity key: keccak256("onyx test entity key 1").
    pub fn test_secret() -> SecretKey {
        SecretKey::from_slice(&keccak256(b"onyx test entity key 1").0).expect("valid scalar")
    }

    /// 20-byte address of a secret key.
    pub fn key_address(sk: &SecretKey) -> [u8; 20] {
        let pk = sk.public_key(&Secp256k1::new()).serialize_uncompressed();
        let mut out = [0u8; 20];
        out.copy_from_slice(&keccak256(&pk[1..]).0[12..]);
        out
    }

    /// EIP-191 sign with `v` in {27, 28} (libsecp256k1 always gives low `s`).
    pub fn eip191_sign(sk: &SecretKey, msg: &[u8]) -> [u8; 65] {
        let digest = Message::from_digest(eip191_digest(msg).0);
        let (recid, rs) = Secp256k1::new()
            .sign_ecdsa_recoverable(&digest, sk)
            .serialize_compact();
        let mut out = [0u8; 65];
        out[..64].copy_from_slice(&rs);
        out[64] = 27 + u8::try_from(recid.to_i32()).expect("recid 0..=3");
        out
    }

    /// Stub proof for namespace `t`: a signature that starts with
    /// `ok:<hex keccak(body ‖ message)>` is valid (any suffix, so size caps are
    /// what refuses long ones), `maybe` is inconclusive, anything else invalid.
    pub struct StubProof;
    impl StubProof {
        pub fn sign(body: &str, msg: &[u8]) -> String {
            let mut b = body.as_bytes().to_vec();
            b.extend_from_slice(msg);
            format!("ok:{}", hex::encode(keccak256(&b).0))
        }
    }
    impl AddressProof for StubProof {
        fn namespace(&self) -> &str {
            "t"
        }
        fn chain(&self) -> &str {
            "t.main"
        }
        fn proof_id(&self) -> &str {
            "onyx.proof.test.v1"
        }
        fn max_window(&self) -> u64 {
            1000
        }
        fn vectors(&self) -> &'static [ProofVector] {
            &[]
        }
        fn verify(&self, body: &str, msg: &[u8], sig: &str) -> ProofOutcome {
            if sig == "maybe" {
                ProofOutcome::Inconclusive
            } else if sig.starts_with(&StubProof::sign(body, msg)) {
                ProofOutcome::Valid
            } else {
                ProofOutcome::Invalid
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;
    use onyx_core::{AddressCanon, EntityKind, StaticRegistry};

    struct AnyCanon;
    impl AddressCanon for AnyCanon {
        fn namespace(&self) -> &str {
            "t"
        }
        fn canon_id(&self) -> &str {
            "onyx.canon.test.v1"
        }
        fn vectors(&self) -> &'static [(&'static str, bool)] {
            &[]
        }
        fn is_canonical(&self, body: &str) -> bool {
            !body.bytes().any(|c| c.is_ascii_uppercase())
        }
    }

    fn reg() -> StaticRegistry {
        let mut r = StaticRegistry::new();
        r.set_domain("onyx.devnet")
            .and_then(|r| r.add_entity("entity:example", EntityKind::Exchange))
            .and_then(|r| r.add_entity("entity:other", EntityKind::Exchange))
            .and_then(|r| r.add_canon(Box::new(AnyCanon)))
            .and_then(|r| r.add_proof(Box::new(StubProof)))
            .and_then(|r| r.add_entity_key("entity:example", key_address(&test_secret())))
            .expect("registry");
        r
    }

    fn statement() -> Statement {
        Statement {
            domain: "onyx.devnet".into(),
            entity: "entity:example".into(),
            address: "t:abc".into(),
            valid_from: 100,
            expires: 200,
            nonce: [7; NONCE_LEN],
        }
    }

    fn attest(st: &Statement) -> Attestation {
        let text = st.render();
        let body = parse_address(&st.address).expect("address").1;
        Attestation {
            address_sig: StubProof::sign(body, text.as_bytes()),
            entity_key: key_address(&test_secret()),
            entity_sig: eip191_sign(&test_secret(), text.as_bytes()),
            statement: text,
        }
    }

    const CTX: AttestContext<'static> = AttestContext {
        ranges: &[("t", 150, 150)],
    };

    #[test]
    fn valid_attestation_verifies() {
        let a = attest(&statement());
        assert_eq!(a.verify(&CTX, &reg()), Ok(statement()));
    }

    #[test]
    fn statement_text_is_exact() {
        assert_eq!(
            statement().render(),
            "Onyx address attestation v1\ndomain: onyx.devnet\nentity: entity:example\naddress: t:abc\nvalid_from: 100\nexpires: 200\nnonce: 07070707070707070707070707070707"
        );
        assert_eq!(Statement::parse(&statement().render()), Ok(statement()));
    }

    /// R-ATTEST-1: one spelling per statement; no injected lines or fields.
    #[test]
    fn non_canonical_statements_rejected() {
        let good = statement().render();
        let bad = [
            format!("{good}\n"),
            format!("{good}\nentity: entity:other"),
            good.replace("\n", "\r\n"),
            good.replace("valid_from: 100", "valid_from: 0100"),
            good.replace("valid_from: 100", "valid_from: +100"),
            good.replace("domain: onyx.devnet", "domain:  onyx.devnet"),
            good.replace("domain: onyx.devnet", "domain: Onyx.devnet"),
            good.replace("07070707", "0707070A"),
            good.replace("07070707070707070707070707070707", "0707"),
            good.replace(
                "entity: entity:example",
                "entity: entity:example\nentity: entity:other",
            ),
            good.replace("address: t:abc", "address: t:a b"),
            good.replace("v1\n", "v2\n"),
            good.replace("valid_from: 100", "valid_from: 200"),
            good.replace("valid_from: 100", "valid_from: 18446744073709551616"),
            good.replace("domain: onyx.devnet\n", ""),
        ];
        for b in bad {
            assert!(Statement::parse(&b).is_err(), "{b:?}");
        }
    }

    #[test]
    fn each_check_fails_closed() {
        let r = reg();
        let ok = attest(&statement());

        // The domain comes from the (pinned) registry.
        let mut other_net = StaticRegistry::new();
        other_net
            .set_domain("onyx.testnet")
            .and_then(|r| r.add_entity("entity:example", EntityKind::Exchange))
            .and_then(|r| r.add_canon(Box::new(AnyCanon)))
            .and_then(|r| r.add_proof(Box::new(StubProof)))
            .and_then(|r| r.add_entity_key("entity:example", key_address(&test_secret())))
            .expect("registry");
        assert_eq!(ok.verify(&CTX, &other_net), Err(AttestError::WrongDomain));

        // The whole range must lie in [valid_from, expires) = [100, 200).
        let at =
            |ranges: &'static [(&'static str, u64, u64)]| ok.verify(&AttestContext { ranges }, &r);
        assert_eq!(at(&[("t", 100, 199)]), Ok(statement()));
        assert_eq!(at(&[("t", 99, 99)]), Err(AttestError::NotYetValid));
        assert_eq!(at(&[("t", 99, 150)]), Err(AttestError::NotYetValid));
        assert_eq!(at(&[("t", 200, 200)]), Err(AttestError::Expired));
        assert_eq!(at(&[("t", 150, 200)]), Err(AttestError::Expired));
        // L14-F7: an inverted range is not a range.
        assert_eq!(at(&[("t", 160, 150)]), Err(AttestError::NoRange));

        let mut st = statement();
        st.entity = "entity:unknown".into();
        assert_eq!(
            attest(&st).verify(&CTX, &r),
            Err(AttestError::UnknownEntity)
        );

        // Registered entity, but the key belongs to entity:example only.
        st = statement();
        st.entity = "entity:other".into();
        assert_eq!(
            attest(&st).verify(&CTX, &r),
            Err(AttestError::EntityKeyUnknown)
        );

        st = statement();
        st.address = "t:ABC".into();
        assert_eq!(
            attest(&st).verify(&CTX, &r),
            Err(AttestError::AddressNotCanonical)
        );

        st = statement();
        st.expires = st.valid_from + 1001;
        assert_eq!(attest(&st).verify(&CTX, &r), Err(AttestError::BadWindow));
        // The proof rule's window (1,000) itself is allowed (mutation L4).
        st = statement();
        st.expires = st.valid_from + 1000;
        assert_eq!(attest(&st).verify(&CTX, &r), Ok(st.clone()));

        // Address signature for another address, or another statement.
        let mut a = ok.clone();
        a.address_sig = StubProof::sign("abd", a.statement.as_bytes());
        assert_eq!(a.verify(&CTX, &r), Err(AttestError::AddressSigInvalid));
        a.address_sig = "maybe".into();
        assert_eq!(a.verify(&CTX, &r), Err(AttestError::AddressSigInconclusive));
        // A valid signature padded past the cap: the stub would accept it, the cap does not.
        a.address_sig = ok.address_sig.clone() + &" ".repeat(MAX_ADDRESS_SIG_LEN);
        assert_eq!(a.verify(&CTX, &r), Err(AttestError::AddressSigInvalid));
        a.address_sig = ok.address_sig.clone() + " ";
        assert_eq!(a.verify(&CTX, &r), Ok(statement()));
        // Exactly at the cap is allowed (mutation L4).
        a.address_sig =
            ok.address_sig.clone() + &" ".repeat(MAX_ADDRESS_SIG_LEN - ok.address_sig.len());
        assert_eq!(a.address_sig.len(), MAX_ADDRESS_SIG_LEN);
        assert_eq!(a.verify(&CTX, &r), Ok(statement()));

        // No range for the namespace.
        let none = AttestContext {
            ranges: &[("u", 150, 150)],
        };
        assert_eq!(ok.verify(&none, &r), Err(AttestError::NoRange));

        // Revoked from 151: fine for a range ending at 150, refused for one ending at 151.
        let mut rr = reg();
        let key = key_address(&test_secret());
        rr.add_revocation("entity:example", "t:abc", 151, key, keccak256(b"r"))
            .expect("rev");
        assert_eq!(ok.verify(&CTX, &rr), Ok(statement()));
        let wide = AttestContext {
            ranges: &[("t", 150, 151)],
        };
        assert_eq!(ok.verify(&wide, &rr), Err(AttestError::Revoked));
    }

    /// The height and nonce grammars on their own (mutation L4: the
    /// re-render check in `Statement::parse` also refuses these spellings).
    #[test]
    fn height_and_nonce_grammars() {
        assert_eq!(parse_height("0"), Ok(0));
        assert_eq!(parse_height("7"), Ok(7));
        assert_eq!(parse_height("18446744073709551615"), Ok(u64::MAX));
        for bad in ["", "07", "+7", "-7", "7a", "000000000000000000001", " 7"] {
            assert_eq!(parse_height(bad), Err(AttestError::Malformed), "{bad:?}");
        }
        let ok = "07".repeat(NONCE_LEN);
        assert_eq!(parse_nonce(&ok), Ok([7; NONCE_LEN]));
        for bad in [
            "07".repeat(NONCE_LEN - 1),
            "0A".repeat(NONCE_LEN),
            "07".repeat(NONCE_LEN) + "07",
        ] {
            assert_eq!(parse_nonce(&bad), Err(AttestError::Malformed), "{bad}");
        }
    }

    /// `eip191_sign_with` signs in the form `eip191_recover` accepts, and
    /// recovers to `secret_address` (mutation L4).
    #[test]
    fn signing_helpers_round_trip() {
        let secret = test_secret().secret_bytes();
        let addr = secret_address(&secret).expect("address");
        assert_eq!(addr, key_address(&test_secret()));
        let sig = eip191_sign_with(&secret, b"msg").expect("sig");
        assert!(sig[64] == 27 || sig[64] == 28);
        assert_eq!(eip191_recover(b"msg", &sig), Some(addr));
        assert_ne!(eip191_recover(b"other", &sig), Some(addr));
        // Both recovery ids occur (v 27 and 28) and each recovers.
        let vs: std::collections::BTreeSet<u8> = (0u8..16)
            .map(|i| {
                let sig = eip191_sign_with(&secret, &[i]).expect("sig");
                assert_eq!(eip191_recover(&[i], &sig), Some(addr));
                sig[64]
            })
            .collect();
        assert_eq!(vs, [27, 28].into_iter().collect());
        assert_eq!(eip191_sign_with(&[0; 32], b"msg"), None);
        assert_eq!(secret_address(&[0; 32]), None);
    }

    #[test]
    fn entity_signature_must_be_canonical_and_match() {
        let r = reg();
        let ok = attest(&statement());

        // Signature over a different statement.
        let mut a = ok.clone();
        let mut other = statement();
        other.nonce = [8; NONCE_LEN];
        a.entity_sig = eip191_sign(&test_secret(), other.render().as_bytes());
        assert_eq!(a.verify(&CTX, &r), Err(AttestError::EntitySigInvalid));

        // v = 0/1 spelling, and a flipped v.
        for v in [ok.entity_sig[64] - 27, ok.entity_sig[64] ^ 0b11, 29] {
            let mut a = ok.clone();
            a.entity_sig[64] = v;
            assert_eq!(
                a.verify(&CTX, &r),
                Err(AttestError::EntitySigInvalid),
                "v={v}"
            );
        }

        // High-s twin: s' = n - s with flipped v recovers the same key, but is refused (EIP-2).
        let mut a = ok.clone();
        let rs = RecoverableSignature::from_compact(
            &ok.entity_sig[..64],
            RecoveryId::from_i32(i32::from(ok.entity_sig[64] - 27)).expect("recid"),
        )
        .expect("sig");
        let n_minus_s = negate_scalar(&rs.serialize_compact().1[32..64]);
        a.entity_sig[32..64].copy_from_slice(&n_minus_s);
        a.entity_sig[64] = if ok.entity_sig[64] == 27 { 28 } else { 27 };
        assert_eq!(a.verify(&CTX, &r), Err(AttestError::EntitySigInvalid));
    }

    /// n - s for the secp256k1 group order n (big-endian, s != 0).
    fn negate_scalar(s: &[u8]) -> [u8; 32] {
        const N: [u8; 32] = [
            0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xfe, 0xba, 0xae, 0xdc, 0xe6, 0xaf, 0x48, 0xa0, 0x3b, 0xbf, 0xd2, 0x5e, 0x8c,
            0xd0, 0x36, 0x41, 0x41,
        ];
        let mut out = [0u8; 32];
        let mut borrow = 0i16;
        for i in (0..32).rev() {
            let d = i16::from(N[i]) - i16::from(s[i]) - borrow;
            borrow = i16::from(d < 0);
            out[i] = (d + (borrow << 8)) as u8;
        }
        out
    }

    /// Cross-check against an independent implementation: vectors made with
    /// Foundry `cast wallet sign` (see fixtures/eip191/README.md).
    #[test]
    fn eip191_matches_cast_vectors() {
        let dir = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/eip191/vectors.json"
        );
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir).expect("vectors")).expect("json");
        let cases = v["cases"].as_array().expect("cases");
        assert!(cases.len() >= 3);
        let want = key_address(&test_secret());
        assert_eq!(
            format!("0x{}", hex::encode(want)),
            v["address"].as_str().expect("address").to_lowercase()
        );
        for c in cases {
            let msg = c["message"].as_str().expect("message");
            let sig_hex = c["signature"].as_str().expect("signature");
            let mut sig = [0u8; 65];
            hex::decode_to_slice(sig_hex.trim_start_matches("0x"), &mut sig).expect("hex");
            assert_eq!(eip191_recover(msg.as_bytes(), &sig), Some(want), "{msg:?}");
            assert_eq!(eip191_sign(&test_secret(), msg.as_bytes()), sig, "{msg:?}");
        }
    }
}
