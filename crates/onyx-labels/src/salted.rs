//! Salted dated label sets, format 3 (partner tracker T1, from a partner's question Q7).
//!
//! A Merkle proof hands a buyer the sibling hashes of the row it bought. In a format-2 dated set
//! a leaf is `keccak256(0x00 || borsh(membership))`, so a buyer who guesses a sibling's
//! membership ("is address X Binance's from height A to B?") can check the guess against that
//! hash. That matters only while a set's rows are not public: a set whose rights are not cleared
//! yet, or a private tier.
//!
//! Format 3 appends a 32-byte salt to each row: `row = borsh(membership) || salt_i`, with
//! `salt_i = keccak256("onyx-row-salt:v1" || set_secret || i as u64 big-endian)`. The Merkle
//! library and the on-chain verifier are unchanged (the salt is part of the row bytes). A buyer
//! receives the salt of the row it bought, never a sibling's, so a guessed sibling cannot be
//! checked. The publisher keeps `set_secret`; validators get the full rows with their salts, so
//! they still check every membership. Format-2 sets keep their bytes, hashes and goldens.

use crate::dated::{DatedLabelSet, Membership};
use borsh::{BorshDeserialize, BorshSerialize};
use onyx_core::{keccak256, to_canonical_bytes, Hash32};
use onyx_merkle::{MerkleTree, Proof};

/// Format number of a salted dated set.
pub const SALTED_FORMAT: u32 = 3;
/// Domain tag of the content hash.
pub const SALTED_DOMAIN: &str = "onyx-salted-label-set-v1";
/// Domain tag of a row salt.
const SALT_TAG: &[u8] = b"onyx-row-salt:v1";

/// The salt of row `index` under `set_secret`.
pub fn row_salt(set_secret: &Hash32, index: u64) -> Hash32 {
    let mut msg = Vec::with_capacity(SALT_TAG.len() + 32 + 8);
    msg.extend_from_slice(SALT_TAG);
    msg.extend_from_slice(&set_secret.0);
    msg.extend_from_slice(&index.to_be_bytes());
    keccak256(&msg)
}

/// The leaf row of one membership: its Borsh bytes, then its salt.
pub fn salted_row(m: &Membership, salt: &Hash32) -> Vec<u8> {
    let mut row = to_canonical_bytes(m).unwrap_or_default();
    row.extend_from_slice(&salt.0);
    row
}

/// The hashed header of a salted set: as format 2, with its own domain and format.
#[derive(BorshSerialize)]
struct SaltedCommit<'a> {
    domain: &'a str,
    format: u32,
    name: &'a str,
    version: u32,
    published: u64,
    members: u64,
    root: Hash32,
}

/// A dated set with one salt per membership. The salts are derived from the publisher's secret;
/// the secret itself is not kept here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaltedDatedSet {
    set: DatedLabelSet,
    salts: Vec<Hash32>,
}

impl SaltedDatedSet {
    /// Salt every membership of `set` under `set_secret`.
    pub fn new(set: DatedLabelSet, set_secret: &Hash32) -> Self {
        let salts = (0..set.members.len() as u64)
            .map(|i| row_salt(set_secret, i))
            .collect();
        Self { set, salts }
    }

    /// The memberships, unchanged.
    pub fn set(&self) -> &DatedLabelSet {
        &self.set
    }

    /// The salt of membership `index`.
    pub fn salt(&self, index: u64) -> Option<Hash32> {
        usize::try_from(index)
            .ok()
            .and_then(|i| self.salts.get(i))
            .copied()
    }

    /// The Merkle rows: each membership's Borsh bytes and its salt, in order.
    pub fn leaves(&self) -> Vec<Vec<u8>> {
        self.set
            .members
            .iter()
            .zip(&self.salts)
            .map(|(m, s)| salted_row(m, s))
            .collect()
    }

    /// Merkle root over [`Self::leaves`].
    pub fn members_root(&self) -> Hash32 {
        MerkleTree::from_rows(&self.leaves()).root()
    }

    /// Content hash: Keccak of the Borsh header (salted domain, format 3, name, version,
    /// publication height, member count, members root).
    pub fn content_hash(&self) -> Hash32 {
        let c = SaltedCommit {
            domain: SALTED_DOMAIN,
            format: SALTED_FORMAT,
            name: &self.set.name,
            version: self.set.version,
            published: self.set.published,
            members: self.set.members.len() as u64,
            root: self.members_root(),
        };
        keccak256(&to_canonical_bytes(&c).unwrap_or_default())
    }

    /// What a buyer of membership `index` receives: the membership, its salt and its proof.
    pub fn member_proof(&self, index: u64) -> Option<(Membership, Hash32, Proof)> {
        let i = usize::try_from(index).ok()?;
        let m = self.set.members.get(i)?.clone();
        let salt = *self.salts.get(i)?;
        let proof = MerkleTree::from_rows(&self.leaves()).proof(index)?;
        Some((m, salt, proof))
    }
}

/// What a buyer of one row receives (ROADMAP D30, pre-public loop PP3): the set's header (so the
/// buyer recomputes the content hash it pinned), the row's index, its bytes (Borsh membership then
/// the row's own salt) and its Merkle proof. Never another row's salt.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct SaltedAnswer {
    /// Set name.
    pub name: String,
    /// Set version.
    pub version: u32,
    /// Publication height.
    pub published: u64,
    /// Member count (the tree's leaf count).
    pub members: u64,
    /// Members root.
    pub root: Hash32,
    /// The row's index.
    pub index: u64,
    /// `borsh(membership) || salt`.
    pub row: Vec<u8>,
    /// The row's proof.
    pub proof: Proof,
}

/// Why a salted answer is refused.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum SaltedError {
    /// The header does not hash to the pinned content hash.
    #[error("the set header does not hash to the pinned content hash")]
    NotPinned,
    /// The proof is for another index or tree size.
    #[error("the proof is for another row or another tree size")]
    ProofMismatch,
    /// The row is not a Borsh membership followed by a 32-byte salt.
    #[error("the row is not a membership followed by its salt")]
    BadRow,
    /// The row does not verify against the root.
    #[error("the row does not verify against the root")]
    NotInRoot,
}

impl SaltedAnswer {
    /// The content hash the header names (as [`SaltedDatedSet::content_hash`]).
    pub fn content_hash(&self) -> Hash32 {
        let c = SaltedCommit {
            domain: SALTED_DOMAIN,
            format: SALTED_FORMAT,
            name: &self.name,
            version: self.version,
            published: self.published,
            members: self.members,
            root: self.root,
        };
        keccak256(&to_canonical_bytes(&c).unwrap_or_default())
    }
}

impl SaltedDatedSet {
    /// The answer for row `index`.
    pub fn answer(&self, index: u64) -> Option<SaltedAnswer> {
        let (m, salt, proof) = self.member_proof(index)?;
        Some(SaltedAnswer {
            name: self.set.name.clone(),
            version: self.set.version,
            published: self.set.published,
            members: self.set.members.len() as u64,
            root: self.members_root(),
            index,
            row: salted_row(&m, &salt),
            proof,
        })
    }

    /// The indices of `address`'s memberships (rows are sorted by address, then start).
    pub fn rows_of(&self, address: &str) -> Vec<u64> {
        let m = &self.set.members;
        let start = m.partition_point(|x| x.label.address.as_str() < address);
        (start..m.len())
            .take_while(|&i| m[i].label.address == address)
            .map(|i| i as u64)
            .collect()
    }
}

/// Check a bought row against the content hash the buyer pinned (from the registry or its own
/// copy, never from the answer): the header hashes to it, the proof is for this row of a tree of
/// `members` leaves, the row is exactly a Borsh membership and a 32-byte salt, and it verifies
/// against the root. Returns the membership and its salt.
pub fn verify_salted_answer(
    a: &SaltedAnswer,
    pinned: &Hash32,
) -> Result<(Membership, Hash32), SaltedError> {
    if &a.content_hash() != pinned {
        return Err(SaltedError::NotPinned);
    }
    if a.proof.index != a.index || a.proof.leaf_count != a.members || a.index >= a.members {
        return Err(SaltedError::ProofMismatch);
    }
    let cut = a.row.len().checked_sub(32).ok_or(SaltedError::BadRow)?;
    let m: Membership = borsh::from_slice(&a.row[..cut]).map_err(|_| SaltedError::BadRow)?;
    let mut salt = [0u8; 32];
    salt.copy_from_slice(&a.row[cut..]);
    if salted_row(&m, &Hash32(salt)) != a.row {
        return Err(SaltedError::BadRow);
    }
    if !onyx_merkle::verify(&a.root, &a.row, &a.proof) {
        return Err(SaltedError::NotInRoot);
    }
    Ok((m, Hash32(salt)))
}

/// Check one salted membership against a salted set's members root.
pub fn verify_salted_membership(
    root: &Hash32,
    m: &Membership,
    salt: &Hash32,
    proof: &Proof,
) -> bool {
    onyx_merkle::verify(root, &salted_row(m, salt), proof)
}

/// The JSON wire form of a [`SaltedAnswer`] (`GET /v1/labels/{set}/{address}`, docs/api/openapi.json
/// `SaltedAnswer`): numbers as JSON integers, hashes and bytes as `0x` lowercase hex.
#[cfg(feature = "json")]
pub mod json {
    use super::{SaltedAnswer, SALTED_FORMAT};
    use onyx_core::Hash32;
    use onyx_merkle::Proof;
    use serde_json::{json, Value};

    /// The answer as JSON, with the content hash its header names.
    pub fn to_json(a: &SaltedAnswer) -> Value {
        json!({
            "format": SALTED_FORMAT,
            "name": a.name,
            "version": a.version,
            "published": a.published,
            "members": a.members,
            "root": a.root.to_string(),
            "contentHash": a.content_hash().to_string(),
            "index": a.index,
            "row": format!("0x{}", hex::encode(&a.row)),
            "proof": {
                "index": a.proof.index,
                "leafCount": a.proof.leaf_count,
                "siblings": a.proof.siblings.iter().map(|h| h.to_string()).collect::<Vec<_>>(),
            },
        })
    }

    fn hash(v: &Value) -> Result<Hash32, String> {
        let s = v.as_str().ok_or("a hash must be a string")?;
        let h = s
            .strip_prefix("0x")
            .filter(|h| h.len() == 64 && h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
            .ok_or("a hash must be 0x and 64 lowercase hex digits")?;
        let mut a = [0u8; 32];
        hex::decode_to_slice(h, &mut a).map_err(|e| e.to_string())?;
        Ok(Hash32(a))
    }

    fn uint(v: &Value, k: &str) -> Result<u64, String> {
        v[k].as_u64()
            .ok_or(format!("`{k}` must be an unsigned integer"))
    }

    /// Parse the wire form; any other key, a wrong format number or a non-canonical hex spelling
    /// is refused. `contentHash` is only checked to equal the header's: a client compares it with
    /// its own pin through [`super::verify_salted_answer`], never trusts it.
    pub fn from_json(v: &Value) -> Result<SaltedAnswer, String> {
        let o = v.as_object().ok_or("the answer is not an object")?;
        const KEYS: [&str; 10] = [
            "contentHash",
            "format",
            "index",
            "members",
            "name",
            "proof",
            "published",
            "root",
            "row",
            "version",
        ];
        if o.len() != KEYS.len() || !KEYS.iter().all(|k| o.contains_key(*k)) {
            return Err("the answer must have exactly its ten keys".into());
        }
        if v["format"] != json!(SALTED_FORMAT) {
            return Err("not a format-3 answer".into());
        }
        let p = v["proof"].as_object().ok_or("`proof` is not an object")?;
        if p.len() != 3 {
            return Err("`proof` must have exactly index, leafCount and siblings".into());
        }
        let row = v["row"]
            .as_str()
            .and_then(|s| s.strip_prefix("0x"))
            .filter(|h| h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
            .ok_or("`row` must be 0x and lowercase hex")?;
        let a = SaltedAnswer {
            name: v["name"]
                .as_str()
                .ok_or("`name` must be a string")?
                .to_string(),
            version: u32::try_from(uint(v, "version")?).map_err(|_| "`version` too large")?,
            published: uint(v, "published")?,
            members: uint(v, "members")?,
            root: hash(&v["root"])?,
            index: uint(v, "index")?,
            row: hex::decode(row).map_err(|e| e.to_string())?,
            proof: Proof {
                index: uint(&v["proof"], "index")?,
                leaf_count: uint(&v["proof"], "leafCount")?,
                siblings: v["proof"]["siblings"]
                    .as_array()
                    .ok_or("`siblings` must be an array")?
                    .iter()
                    .map(hash)
                    .collect::<Result<_, _>>()?,
            },
        };
        if hash(&v["contentHash"])? != a.content_hash() {
            return Err("`contentHash` is not the header's".into());
        }
        Ok(a)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dated::verify_membership;
    use crate::Label;
    use onyx_core::{Confidence, EntityKind};
    use onyx_merkle::leaf_hash;

    fn m(addr: &str, entity: &str, from: u64, to: Option<u64>) -> Membership {
        Membership {
            label: Label {
                address: addr.into(),
                entity: entity.into(),
                kind: EntityKind::Exchange,
                confidence: Confidence(900_000),
                evidence: vec![],
            },
            from,
            to,
        }
    }

    fn set() -> DatedLabelSet {
        DatedLabelSet {
            name: "onyx.test".into(),
            version: 1,
            published: 1_000,
            members: vec![
                m("t:a", "entity:a", 100, None),
                m("t:b", "entity:b", 200, Some(299)),
                m("t:c", "entity:a", 150, None),
            ],
        }
    }

    const SECRET: Hash32 = Hash32([7; 32]);

    #[test]
    fn a_bought_row_verifies_with_its_salt_only() {
        let s = SaltedDatedSet::new(set(), &SECRET);
        let root = s.members_root();
        for i in 0..3 {
            let (mem, salt, proof) = s.member_proof(i).expect("proof");
            assert!(verify_salted_membership(&root, &mem, &salt, &proof));
            assert!(!verify_salted_membership(
                &root,
                &mem,
                &Hash32::ZERO,
                &proof
            ));
            assert!(
                !verify_membership(&root, &mem, &proof),
                "an unsalted row must not verify"
            );
        }
        assert!(s.member_proof(3).is_none());
    }

    #[test]
    fn a_guessed_sibling_can_be_checked_in_format_2_but_not_in_format_3() {
        // The attack in Q7: the buyer of row 0 holds the sibling hash of row 1 and guesses row 1
        // exactly (address, entity and range).
        let guess = m("t:b", "entity:b", 200, Some(299));

        let plain = set();
        let proof2 = plain.member_proof(0).expect("proof");
        let guess_leaf = leaf_hash(&to_canonical_bytes(&guess).expect("borsh"));
        assert_eq!(
            proof2.siblings[0], guess_leaf,
            "format 2: the guess is confirmed"
        );

        let salted = SaltedDatedSet::new(set(), &SECRET);
        let (_, _, proof3) = salted.member_proof(0).expect("proof");
        assert_ne!(proof3.siblings[0], guess_leaf);
        assert_ne!(
            proof3.siblings[0],
            leaf_hash(&salted_row(&guess, &Hash32::ZERO))
        );
        // Only the sibling's own salt, which the buyer never receives, reproduces it.
        let s1 = salted.salt(1).expect("salt");
        assert_eq!(proof3.siblings[0], leaf_hash(&salted_row(&guess, &s1)));
    }

    #[test]
    fn salts_are_per_row_per_secret_and_deterministic() {
        let a = SaltedDatedSet::new(set(), &SECRET);
        let b = SaltedDatedSet::new(set(), &SECRET);
        let c = SaltedDatedSet::new(set(), &Hash32([8; 32]));
        assert_eq!(a.members_root(), b.members_root());
        assert_ne!(a.members_root(), c.members_root());
        assert_ne!(a.salt(0), a.salt(1));
        assert_eq!(a.salt(2), Some(row_salt(&SECRET, 2)));
    }

    #[test]
    fn format_3_has_its_own_content_hash_and_row_shape() {
        let plain = set();
        let salted = SaltedDatedSet::new(plain.clone(), &SECRET);
        assert_ne!(salted.content_hash(), plain.content_hash());
        assert_eq!(salted.set(), &plain);
        let borsh_len = to_canonical_bytes(&plain.members[0]).expect("borsh").len();
        assert_eq!(salted.leaves()[0].len(), borsh_len + 32);
    }

    #[test]
    fn an_answer_verifies_against_its_pin_and_nothing_else() {
        let s = SaltedDatedSet::new(set(), &SECRET);
        let pin = s.content_hash();
        for i in 0..3 {
            let a = s.answer(i).expect("answer");
            let (m, salt) = verify_salted_answer(&a, &pin).expect("verifies");
            assert_eq!(m, s.set().members[i as usize]);
            assert_eq!(Some(salt), s.salt(i));
        }
        assert!(s.answer(3).is_none());
        let a = s.answer(1).expect("answer");
        let refused = |f: &dyn Fn(&mut SaltedAnswer), want: SaltedError| {
            let mut b = a.clone();
            f(&mut b);
            assert_eq!(verify_salted_answer(&b, &pin), Err(want));
        };
        refused(&|b| b.version += 1, SaltedError::NotPinned);
        refused(&|b| b.root = Hash32([1; 32]), SaltedError::NotPinned);
        refused(&|b| b.index = 0, SaltedError::ProofMismatch);
        refused(&|b| b.proof.leaf_count = 4, SaltedError::ProofMismatch);
        refused(&|b| b.row.truncate(20), SaltedError::BadRow);
        refused(&|b| b.row.push(0), SaltedError::BadRow);
        refused(
            &|b| *b.row.last_mut().expect("salt") ^= 1,
            SaltedError::NotInRoot,
        );
        // Another row of the same set under this row's index: refused.
        refused(
            &|b| b.row = s.answer(2).expect("row 2").row,
            SaltedError::NotInRoot,
        );
        assert_eq!(
            verify_salted_answer(&a, &Hash32::ZERO),
            Err(SaltedError::NotPinned)
        );
    }

    #[test]
    fn rows_of_finds_every_range_of_an_address() {
        let mut d = set();
        d.members.insert(2, m("t:b", "entity:b", 300, None));
        let s = SaltedDatedSet::new(d, &SECRET);
        assert_eq!(s.rows_of("t:a"), vec![0]);
        assert_eq!(s.rows_of("t:b"), vec![1, 2]);
        assert_eq!(s.rows_of("t:c"), vec![3]);
        assert!(s.rows_of("t:bb").is_empty());
        assert!(s.rows_of("").is_empty());
    }

    /// Shared vectors (pre-public loop PP3): a synthetic salted set (example entities, `test:`
    /// addresses), every row's answer and the refusals, run by the node's tests and the browser
    /// verifier. Regenerate only with a format change: `ONYX_BLESS=1 cargo test -p onyx-labels`.
    #[test]
    fn salted_answer_vectors() {
        use serde_json::json;
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/dated/salted-vectors.json"
        );
        let secret = keccak256(b"onyx-test-salted-secret-v1");
        let d = DatedLabelSet {
            name: "test.onyx.salted".into(),
            version: 1,
            published: 2_000,
            members: vec![
                m("test:addr-1", "entity:example-a", 100, None),
                m("test:addr-2", "entity:example-b", 200, Some(499)),
                m("test:addr-2", "entity:example-a", 500, None),
                m("test:addr-3", "entity:example-b", 150, None),
            ],
        };
        let s = SaltedDatedSet::new(d, &secret);
        let pin = s.content_hash();
        let mut cases = Vec::new();
        for i in 0..4 {
            let a = s.answer(i).expect("answer");
            let (mem, _) = verify_salted_answer(&a, &pin).expect("ok");
            cases.push(json!({"case": format!("row {i}"), "answer": json::to_json(&a), "expect": "ok",
                "expectAddress": mem.label.address, "expectEntity": mem.label.entity, "from": mem.from, "to": mem.to}));
        }
        let a1 = s.answer(1).expect("answer");
        let mut tampered = |name: &str, f: &dyn Fn(&mut SaltedAnswer), want: &str| {
            let mut b = a1.clone();
            f(&mut b);
            let got = match verify_salted_answer(&b, &pin) {
                Ok(_) => "ok".to_string(),
                Err(e) => format!("{e:?}"),
            };
            assert_eq!(got, want, "{name}");
            cases.push(json!({"case": name, "answer": json::to_json(&b), "expect": want}));
        };
        tampered(
            "another version (another pin)",
            &|b| b.version = 2,
            "NotPinned",
        );
        tampered("proof of another index", &|b| b.index = 0, "ProofMismatch");
        tampered("row cut short", &|b| b.row.truncate(16), "BadRow");
        tampered(
            "salt bit flipped",
            &|b| *b.row.last_mut().expect("salt") ^= 1,
            "NotInRoot",
        );
        tampered(
            "another row's bytes",
            &|b| b.row = s.answer(3).expect("row 3").row,
            "NotInRoot",
        );
        // a partner's Q7: the buyer of row 0 guesses its neighbour exactly; the sibling does not confirm it.
        let a0 = s.answer(0).expect("row 0");
        let guess = s.set().members[1].clone();
        let guess_leaf = onyx_merkle::leaf_hash(&to_canonical_bytes(&guess).expect("borsh"));
        assert_ne!(a0.proof.siblings[0], guess_leaf);
        let got = json!({
            "format": SALTED_FORMAT,
            "domain": SALTED_DOMAIN,
            "synthetic": "example entities and test: addresses only; no real label data",
            "saltSeedHex": format!("0x{}", hex::encode(secret.0)),
            "contentHash": pin.to_string(),
            "cases": cases,
            "guess": {"row": format!("0x{}", hex::encode(to_canonical_bytes(&guess).expect("borsh"))),
                      "leafIfUnsalted": guess_leaf.to_string(), "sibling": a0.proof.siblings[0].to_string()},
        });
        if std::env::var_os("ONYX_BLESS").is_some() {
            std::fs::write(
                path,
                serde_json::to_string_pretty(&got).expect("json") + "\n",
            )
            .expect("write");
        }
        let want: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(path)
                .expect("fixtures/dated/salted-vectors.json missing; run with ONYX_BLESS=1"),
        )
        .expect("json");
        assert_eq!(got, want);
        // The wire form round-trips and refuses an extra key or a changed content hash.
        let w = json::to_json(&a1);
        assert_eq!(json::from_json(&w), Ok(a1.clone()));
        let mut extra = w.clone();
        extra["entity"] = "entity:example-a".into();
        assert!(json::from_json(&extra).is_err());
        let mut lie = w.clone();
        lie["contentHash"] = Hash32([3; 32]).to_string().into();
        assert!(json::from_json(&lie).is_err());
        let mut upper = w;
        upper["root"] = upper["root"]
            .as_str()
            .expect("root")
            .to_uppercase()
            .replace("0X", "0x")
            .into();
        assert!(json::from_json(&upper).is_err());
    }
}
