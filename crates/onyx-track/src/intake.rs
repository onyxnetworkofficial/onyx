//! The statement intake (ADR-0036 amendment 1, docs/spec/proof-ladder.md
//! "Intake"; ROADMAP C5).
//!
//! A statement enters a track record only as a [`SignedStatement`]: signed
//! with EIP-191 by a registered key of its source ([`KeyBook`], the source's
//! reviewed level-3 keys; PLAN q-TR-2). The intake seals pending statements
//! into a batch (a Merkle tree under a [`BatchHeader`]) and posts only the
//! header's hash to a [`FinalizedRecord`]. Every statement of a batch gets
//! `committed_at` = the timestamp of the finalized block carrying its post:
//! chain time, never a caller's clock. A statement stays sealed (only the
//! root is public) until `max(committed_at, close)`, so nobody can copy it
//! before its close (TR-7).
//!
//! A client rechecks all of it with [`verify_log`]: posts `0..n` chained,
//! every revealed statement proven under its root and signed by an active key
//! of its source in its batch's book, revealed no earlier than allowed, no
//! payload twice, complete batches past their reveal, and the commitment's
//! log equal to what was revealed, with the post times.
//!
//! Pure and deterministic: no I/O, no clock. The chain side lives behind
//! [`FinalizedRecord`] (`onyx_coordinator::track_chain::CastRecord` today; the
//! C2 signed-source book later). Nothing here touches proof levels or money
//! (ADR-0036 item 12).

use crate::{admissible, check_source, hash_with, sort_statements, Fact, Statement, TrackError};
use crate::{TrackParams, MAX_ITEMS};
use borsh::{BorshDeserialize, BorshSerialize};
use onyx_core::signed::KEY_DOMAIN;
use onyx_core::{keccak256, Confidence, Hash32, Registry, SourceId, SourceKey, Value};
use onyx_labels::attest::eip191_recover;
use onyx_merkle::{MerkleTree, Proof};
use std::collections::{BTreeMap, BTreeSet};

/// Domain tag of [`IntakeId::id`].
pub const INTAKE_DOMAIN: &[u8] = b"onyx-track-intake-v1";
/// Domain tag of [`StatementPayload::digest`]; also the prefix of the signed text.
pub const SIGNED_STATEMENT_DOMAIN: &str = "onyx-track-signed-statement-v1";
/// Domain tag of [`KeyBook::hash`].
pub const KEYS_DOMAIN: &[u8] = b"onyx-track-keys-v1";
/// Domain tag of [`BatchHeader::hash`].
pub const BATCH_DOMAIN: &[u8] = b"onyx-track-batch-v1";
/// Domain tag of [`Receipt::digest`]; also the prefix of the signed text.
pub const RECEIPT_DOMAIN: &str = "onyx-track-intake-receipt-v1";
/// Most pending statements of one source in one batch (a size bound against
/// floods, not a scoring rule; amendment 1 item 8).
pub const MAX_PENDING_PER_SOURCE: usize = 4_096;
/// Most statements in one batch.
pub const MAX_BATCH: usize = 1 << 16;

/// One intake deployment. Its [`IntakeId::id`] is in every payload, so a
/// statement signed for one intake never replays into another.
#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct IntakeId {
    /// EVM chain id of the record.
    pub chain_id: u64,
    /// Address the posts go to.
    pub sink: [u8; 20],
    /// The dedicated account that posts (its nonce counts the posts).
    pub poster: [u8; 20],
    /// The poster's nonce before its first post: post `seq` has nonce
    /// `first_nonce + seq`. Bound here so nobody can start the count later
    /// and hide earlier posts.
    pub first_nonce: u64,
}

impl IntakeId {
    /// `keccak256("onyx-track-intake-v1" ‖ borsh(id))`.
    pub fn id(&self) -> Hash32 {
        hash_with(INTAKE_DOMAIN, self)
    }
}

/// What a source signs: a statement without its commit time (the intake
/// takes that from the chain).
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct StatementPayload {
    /// [`IntakeId::id`] of the intake it is for.
    pub intake: Hash32,
    /// [`TrackKey::id`] of the signing key (binds key and source).
    pub key: Hash32,
    /// The registered source stating it.
    pub source: SourceId,
    /// The fact.
    pub fact: Fact,
    /// The stated value.
    pub value: Value,
    /// Stated probability (ppm) that the value will agree.
    pub confidence: Confidence,
}

impl StatementPayload {
    /// `keccak256("onyx-track-signed-statement-v1" ‖ borsh(payload))`.
    pub fn digest(&self) -> Hash32 {
        hash_with(SIGNED_STATEMENT_DOMAIN.as_bytes(), self)
    }

    /// The exact text the key signs with EIP-191 personal_sign:
    /// `onyx-track-signed-statement-v1:0x<digest hex>`.
    pub fn message(&self) -> String {
        format!("{SIGNED_STATEMENT_DOMAIN}:0x{}", self.digest().to_hex())
    }

    /// The track statement it becomes once committed at `committed_at`.
    pub fn statement(&self, committed_at: u64) -> Statement {
        Statement {
            source: self.source.clone(),
            fact: self.fact.clone(),
            value: self.value.clone(),
            confidence: self.confidence,
            committed_at,
        }
    }
}

/// A payload and its 65-byte EIP-191 signature `r ‖ s ‖ v`.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct SignedStatement {
    /// What was signed.
    pub payload: StatementPayload,
    /// Signature over [`StatementPayload::message`] (canonical: `v` 27 or 28, low `s`).
    pub signature: [u8; 65],
}

impl SignedStatement {
    /// The address that signed it, if the signature is canonical.
    pub fn signer(&self) -> Option<[u8; 20]> {
        eip191_recover(self.payload.message().as_bytes(), &self.signature)
    }

    /// Its Borsh row, the leaf of its batch's tree. The row includes the
    /// signature, which nobody without the key can predict, so the root
    /// hides even a `Bool` value until the statement is revealed (TR-7).
    pub fn row(&self) -> Vec<u8> {
        borsh::to_vec(self).unwrap_or_default()
    }
}

/// The id of (source, signer): the same as `onyx_core::SourceKey::id`, so a
/// level-3 key and its track key are one key.
pub fn key_id(source: &SourceId, signer: &[u8; 20]) -> Hash32 {
    let mut b = KEY_DOMAIN.to_vec();
    b.extend_from_slice(&borsh::to_vec(&(source, signer)).unwrap_or_default());
    keccak256(&b)
}

/// A key a source signs statements with (its reviewed level-3 key).
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct TrackKey {
    /// The registered source.
    pub source: SourceId,
    /// EIP-191 signer address.
    pub signer: [u8; 20],
    /// Revoked (retired or compromised in the level-3 registry): signs no
    /// new statement. Never cleared.
    pub revoked: bool,
}

impl TrackKey {
    /// [`key_id`] of its source and signer.
    pub fn id(&self) -> Hash32 {
        key_id(&self.source, &self.signer)
    }
}

/// The keys in force for one batch: strictly ascending by [`TrackKey::id`].
#[derive(Clone, Debug, Default, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct KeyBook {
    /// The keys.
    pub keys: Vec<TrackKey>,
}

impl KeyBook {
    /// `keccak256("onyx-track-keys-v1" ‖ borsh(book))`.
    pub fn hash(&self) -> Hash32 {
        hash_with(KEYS_DOMAIN, self)
    }

    /// Shape and registry check: ascending ids, non-zero signers, sources
    /// registered with ingestible rights, one key per signer, and every key a
    /// reviewed level-3 key in `reviewed` (a well-formed `SourceKey` of the
    /// same source and signer, with the provenance where the source published
    /// it): the intake cannot add a key of its own (review TR-R3-2).
    pub fn validate(&self, reg: &dyn Registry, reviewed: &[SourceKey]) -> Result<(), IntakeError> {
        if self.keys.len() > MAX_ITEMS {
            return Err(IntakeError::BadKeys("too many keys"));
        }
        let ids: Vec<Hash32> = self.keys.iter().map(TrackKey::id).collect();
        if !ids.windows(2).all(|w| w[0] < w[1]) {
            return Err(IntakeError::BadKeys("not strictly ascending"));
        }
        let mut signers = BTreeSet::new();
        for k in &self.keys {
            if k.signer == [0u8; 20] {
                return Err(IntakeError::BadKeys("zero signer"));
            }
            if !signers.insert(k.signer) {
                return Err(IntakeError::BadKeys("signer under two sources"));
            }
        }
        for k in &self.keys {
            check_source(reg, &k.source)?;
            if !reviewed
                .iter()
                .any(|r| r.source == k.source && r.signer == k.signer && r.validate().is_ok())
            {
                return Err(IntakeError::BadKeys("not a reviewed level-3 key"));
            }
        }
        Ok(())
    }

    /// True if `self` keeps every key of `earlier` and never un-revokes one
    /// (keys only move forward; amendment 1 item 5).
    pub fn extends(&self, earlier: &KeyBook) -> bool {
        earlier.keys.iter().all(|old| {
            self.find(&old.id())
                .is_some_and(|k| k.revoked || !old.revoked)
        })
    }

    fn find(&self, id: &Hash32) -> Option<&TrackKey> {
        self.keys
            .binary_search_by(|k| k.id().cmp(id))
            .ok()
            .map(|i| &self.keys[i])
    }

    /// Check `s`'s key and signature against this book (R-TI-1).
    pub fn check(&self, s: &SignedStatement) -> Result<(), IntakeError> {
        let key = self.find(&s.payload.key).ok_or(IntakeError::UnknownKey)?;
        if key.source != s.payload.source {
            return Err(IntakeError::KeyNotSource);
        }
        if key.revoked {
            return Err(IntakeError::KeyRevoked);
        }
        if s.signer() != Some(key.signer) {
            return Err(IntakeError::BadSignature);
        }
        Ok(())
    }
}

/// A key's level-3 status in the signed-source book (`OnyxSignedBook.keyState`)
/// at the end of a post's block (ADR-0036 amendment 4 item 3, THREATS TR-R4-4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum KeyLife {
    /// Active and not suspended: its statements in the batch count.
    Active,
    /// Active but suspended (an equivocation the reviewer has not cleared).
    Suspended,
    /// Retired by the reviewer.
    Retired,
    /// Marked compromised.
    Compromised,
    /// Not registered in the book.
    Unknown,
}

/// The level-3 status of every key of one batch's book at its post's block,
/// strictly ascending by key id. A revealed statement enters the log only if
/// its key is [`KeyLife::Active`] there: the status a client reads from the
/// chain, not the coordinator's `revoked` flag, so a key the level-3 book
/// marks compromised, retired or suspended stops counting at the first post
/// after it, whatever the coordinator's book says. Read at the post's own
/// block, so a revocation and a post race to one deterministic outcome.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct KeyLives {
    /// The post's block number.
    pub block: u64,
    /// (key id, status), strictly ascending by id, exactly the book's keys.
    pub keys: Vec<(Hash32, KeyLife)>,
}

impl KeyLives {
    /// Every key of `book` active at `block`: for an intake whose keys have no
    /// level-3 book on chain (tests, a local record), where the coordinator's
    /// `revoked` flags are the only status (trusted, as before C9).
    pub fn all_active(book: &KeyBook, block: u64) -> Self {
        KeyLives {
            block,
            keys: book
                .keys
                .iter()
                .map(|k| (k.id(), KeyLife::Active))
                .collect(),
        }
    }

    /// Is this the status list of `book` at `block` (same ids, in order)?
    fn covers(&self, book: &KeyBook, block: u64) -> bool {
        self.block == block
            && self.keys.len() == book.keys.len()
            && self.keys.iter().zip(&book.keys).all(|(l, k)| l.0 == k.id())
    }

    fn active(&self, key: &Hash32) -> bool {
        self.keys
            .binary_search_by(|(id, _)| id.cmp(key))
            .is_ok_and(|i| self.keys[i].1 == KeyLife::Active)
    }
}

/// The intake's promise to post a statement by a deadline (ADR-0036
/// amendment 4 item 5, THREATS TR-I-6): signed with EIP-191 by the intake's
/// dedicated poster account (`IntakeId.poster`, which every payload's intake id
/// names), handed to the source when its statement is taken. It never changes
/// a commit time (that stays the post's block time, so a colluding intake
/// cannot backdate with a receipt); it makes a delay or a drop attributable:
/// [`check_receipt`] shows anyone that a statement the intake took was posted
/// late or never.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Receipt {
    /// [`IntakeId::id`].
    pub intake: Hash32,
    /// [`StatementPayload::digest`] of the statement taken.
    pub digest: Hash32,
    /// Chain time (unix seconds) by which a finalized post carries it; at most
    /// the statement's close, so a kept receipt is never `Late`.
    pub deadline: u64,
}

impl Receipt {
    /// `keccak256("onyx-track-intake-receipt-v1" ‖ borsh(receipt))`.
    pub fn digest(&self) -> Hash32 {
        hash_with(RECEIPT_DOMAIN.as_bytes(), self)
    }

    /// The exact text the poster signs: `onyx-track-intake-receipt-v1:0x<digest hex>`.
    pub fn message(&self) -> String {
        format!("{RECEIPT_DOMAIN}:0x{}", self.digest().to_hex())
    }
}

/// A receipt and the poster's 65-byte EIP-191 signature `r ‖ s ‖ v`.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct SignedReceipt {
    /// What was promised.
    pub receipt: Receipt,
    /// Signature over [`Receipt::message`] (canonical: `v` 27 or 28, low `s`).
    pub signature: [u8; 65],
}

/// What a receipt shows at a finalized head ([`check_receipt`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceiptStatus {
    /// Posted in batch `seq` at `committed_at`, at or before the deadline.
    Kept {
        /// The batch.
        seq: u64,
        /// Its post's block time.
        committed_at: u64,
    },
    /// Posted in batch `seq`, but after the deadline: the intake broke it.
    PostedLate {
        /// The batch.
        seq: u64,
        /// Its post's block time.
        committed_at: u64,
    },
    /// Not shown in any batch, and the head is past both the deadline and
    /// the statement's close (by when an on-time post would be revealed): the
    /// intake broke it.
    Broken,
    /// Not shown yet, and an on-time post could still be sealed.
    Pending,
}

/// What is posted, by its hash: one batch of sealed statements.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct BatchHeader {
    /// [`IntakeId::id`].
    pub intake: Hash32,
    /// 0, 1, 2, ... (the post's position).
    pub seq: u64,
    /// The previous header's hash (zero for the first).
    pub prev: Hash32,
    /// [`KeyBook::hash`] of the book its statements were checked against.
    pub keys: Hash32,
    /// `onyx-merkle` root over the statements' rows, sorted ascending.
    pub statements_root: Hash32,
    /// Rows in it.
    pub statement_count: u64,
    /// The reveal time every one of its statements shares (one per batch,
    /// TR-R3-1; [`TrackParams::reveal_of`]): by `max(post time, reveal_by)`
    /// every statement is revealed.
    pub reveal_by: u64,
}

impl BatchHeader {
    /// `keccak256("onyx-track-batch-v1" ‖ borsh(header))`: the 32 bytes posted.
    pub fn hash(&self) -> Hash32 {
        hash_with(BATCH_DOMAIN, self)
    }
}

/// A finalized block.
#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Anchor {
    /// Block number.
    pub block: u64,
    /// Block hash.
    pub block_hash: Hash32,
    /// Block timestamp (unix seconds): the chain's clock.
    pub timestamp: u64,
}

/// One post as the record shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct Post {
    /// Its position (0, 1, ...).
    pub seq: u64,
    /// The 32 bytes it carried (a [`BatchHeader::hash`]).
    pub header_hash: Hash32,
    /// The block it landed in.
    pub anchor: Anchor,
}

/// The chain interface (amendment 1 item 10). Implementations: a
/// transaction from a dedicated poster to a sink, counted by the poster's
/// nonce (`onyx_coordinator::track_chain::CastRecord`); the C2 signed-source
/// book, counted by the contract.
pub trait FinalizedRecord {
    /// The finalized head: nothing at or below it can change.
    fn head(&self) -> Result<Anchor, String>;
    /// Every post at or below `head`, in order. Must fail when the record
    /// shows more posts than it returns (a hidden post).
    fn posts(&self, head: &Anchor) -> Result<Vec<Post>, String>;
}

/// A batch as published at some head: its header and the statements revealed
/// so far, each with its proof under `statements_root`.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct RevealedBatch {
    /// The header.
    pub header: BatchHeader,
    /// Revealed statements with their proofs.
    pub statements: Vec<(SignedStatement, Proof)>,
}

/// Refused statement, key book or log.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum IntakeError {
    /// Not admissible under the track params (R-TR-1).
    #[error(transparent)]
    Track(#[from] TrackError),
    /// Signed for another intake.
    #[error("statement is for another intake")]
    WrongIntake,
    /// The key is not in the book.
    #[error("unknown key")]
    UnknownKey,
    /// The key belongs to another source.
    #[error("key does not belong to the statement's source")]
    KeyNotSource,
    /// The key is revoked.
    #[error("key revoked")]
    KeyRevoked,
    /// The signature does not recover to the key's signer.
    #[error("signature does not match the key")]
    BadSignature,
    /// This payload was taken before.
    #[error("statement already taken (replay)")]
    Replay,
    /// Its close is at or before the latest finalized time.
    #[error("the fact's close has passed")]
    PastClose,
    /// Too many pending statements (per source or per batch).
    #[error("too many pending statements")]
    Flood,
    /// A malformed key book.
    #[error("bad key book: {0}")]
    BadKeys(&'static str),
    /// A key book that drops a key or un-revokes one.
    #[error("key book does not extend the one before")]
    KeysNotForward,
    /// A post that does not match its batch's header.
    #[error("post does not match its batch")]
    PostMismatch,
    /// The record shows a post the intake did not seal, or a post is missing.
    #[error("the record shows a post outside the intake's batches")]
    HiddenPost,
    /// Posts or headers out of order or not chained.
    #[error("batch chain broken: {0}")]
    BadChain(&'static str),
    /// A revealed statement not proven under its batch root.
    #[error("revealed statement not under its batch root")]
    BadProof,
    /// A statement revealed before its reveal time.
    #[error("statement revealed before its close")]
    EarlyReveal,
    /// A batch past its reveal time with statements still sealed.
    #[error("batch not fully revealed")]
    Incomplete,
    /// The log is not the revealed statements with their post times.
    #[error("log differs from the revealed statements")]
    LogMismatch,
    /// The commitment's `as_of` is not the head's timestamp.
    #[error("as_of is not the finalized head's time")]
    NotHead,
    /// The record could not be read.
    #[error("record: {0}")]
    Record(String),
}

/// A sealed batch, as the intake keeps it (statements private until reveal).
#[derive(Clone, Debug)]
struct Sealed {
    header: BatchHeader,
    rows: Vec<SignedStatement>,
    reveals: Vec<u64>,
    tree: MerkleTree,
    anchor: Option<Anchor>,
}

/// The coordinator's intake: key book, pending statements, sealed batches.
///
/// Deliberately not `Clone` (one intake per deployment, as
/// `SignedSources`): nobody tries a statement against copies.
#[derive(Debug)]
pub struct Intake {
    id: Hash32,
    book: KeyBook,
    books: Vec<KeyBook>,
    /// (statement, close, reveal).
    pending: Vec<(SignedStatement, u64, u64)>,
    per_source: BTreeMap<SourceId, usize>,
    seen: BTreeSet<Hash32>,
    batches: Vec<Sealed>,
}

impl Intake {
    /// An empty intake with id `id` and key book `book` (its keys from the
    /// level-3 registry's reviewed keys `reviewed`).
    pub fn new(
        id: Hash32,
        book: KeyBook,
        reg: &dyn Registry,
        reviewed: &[SourceKey],
    ) -> Result<Self, IntakeError> {
        book.validate(reg, reviewed)?;
        Ok(Intake {
            id,
            book,
            books: Vec::new(),
            pending: Vec::new(),
            per_source: BTreeMap::new(),
            seen: BTreeSet::new(),
            batches: Vec::new(),
        })
    }

    /// Its id.
    pub fn id(&self) -> Hash32 {
        self.id
    }

    /// Replace the key book from the next [`Intake::seal`] on. It must
    /// extend the current one (R-TI-7). Pending statements whose key it
    /// revokes are dropped (returned count); sealed ones stand.
    pub fn set_keys(
        &mut self,
        book: KeyBook,
        reg: &dyn Registry,
        reviewed: &[SourceKey],
    ) -> Result<usize, IntakeError> {
        book.validate(reg, reviewed)?;
        if !book.extends(&self.book) {
            return Err(IntakeError::KeysNotForward);
        }
        let before = self.pending.len();
        self.pending.retain(|(s, _, _)| book.check(s).is_ok());
        self.book = book;
        self.recount();
        Ok(before - self.pending.len())
    }

    fn recount(&mut self) {
        self.per_source.clear();
        for (s, _, _) in &self.pending {
            *self.per_source.entry(s.payload.source.clone()).or_default() += 1;
        }
    }

    /// The receipt for a statement taken with [`Intake::submit`] (its
    /// `digest`), promising a post by `deadline`, for the poster to sign
    /// (R-TI-10). `None` unless the statement is pending here and `deadline`
    /// is at or before its close.
    pub fn receipt(&self, digest: &Hash32, deadline: u64) -> Option<Receipt> {
        self.pending
            .iter()
            .find(|(s, close, _)| s.payload.digest() == *digest && deadline <= *close)
            .map(|_| Receipt {
                intake: self.id,
                digest: *digest,
                deadline,
            })
    }

    /// Where a sealed statement is, for its source before the reveal: the
    /// batch `seq` and the proof of its row under that batch's root. The
    /// source checks it against the posted header ([`check_receipt`]'s
    /// `inclusion`), so a kept receipt is provable before anything is public.
    pub fn inclusion(&self, digest: &Hash32) -> Option<(u64, Proof)> {
        self.batches.iter().find_map(|b| {
            let i = b.rows.iter().position(|s| s.payload.digest() == *digest)?;
            b.tree.proof(i as u64).map(|p| (b.header.seq, p))
        })
    }

    /// Take one signed statement (R-TI-1..3). `final_time` is the latest
    /// finalized block time the intake knows: a statement whose close is at
    /// or before it is refused (`PastClose`). Returns the payload digest.
    pub fn submit(
        &mut self,
        params: &TrackParams,
        reg: &dyn Registry,
        s: SignedStatement,
        final_time: u64,
    ) -> Result<Hash32, IntakeError> {
        if s.payload.intake != self.id || params.intake != self.id {
            return Err(IntakeError::WrongIntake);
        }
        self.book.check(&s)?;
        let digest = s.payload.digest();
        if self.seen.contains(&digest) {
            return Err(IntakeError::Replay);
        }
        let close = admissible(params, reg, &s.payload.statement(0))?;
        if close <= final_time {
            return Err(IntakeError::PastClose);
        }
        let reveal = params
            .reveal_of(&s.payload.fact)
            .ok_or(IntakeError::Track(TrackError::BadPeriod))?;
        let mine = self.per_source.get(&s.payload.source).copied().unwrap_or(0);
        if mine >= MAX_PENDING_PER_SOURCE || self.pending.len() >= MAX_BATCH {
            return Err(IntakeError::Flood);
        }
        self.seen.insert(digest);
        *self.per_source.entry(s.payload.source.clone()).or_default() += 1;
        self.pending.push((s, close, reveal));
        Ok(digest)
    }

    /// Seal the pending statements with the earliest reveal time into the
    /// next batch (R-TI-4): every statement of a batch has the same reveal
    /// time ([`TrackParams::reveal_of`]: its close, or a period fact's
    /// `as_of`), the header's `reveal_by`, so none can stay sealed past its own
    /// (review TR-R3-1). `None` when nothing is pending; call again until
    /// `None`, posting each `header.hash()` in order.
    pub fn seal(&mut self) -> Option<BatchHeader> {
        let reveal = self.pending.iter().map(|(_, _, r)| *r).min()?;
        let (mut pending, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.pending)
            .into_iter()
            .partition(|(_, _, r)| *r == reveal);
        self.pending = rest;
        self.recount();
        pending.sort_by_key(|(s, _, _)| s.row());
        let rows: Vec<Vec<u8>> = pending.iter().map(|(s, _, _)| s.row()).collect();
        let tree = MerkleTree::from_rows(&rows);
        let header = BatchHeader {
            intake: self.id,
            seq: self.batches.len() as u64,
            prev: self
                .batches
                .last()
                .map_or(Hash32::ZERO, |b| b.header.hash()),
            keys: self.book.hash(),
            statements_root: tree.root(),
            statement_count: tree.leaf_count(),
            reveal_by: reveal,
        };
        if !self.books.iter().any(|b| b.hash() == header.keys) {
            self.books.push(self.book.clone());
        }
        let (rows, reveals) = pending.into_iter().map(|(s, _, r)| (s, r)).unzip();
        self.batches.push(Sealed {
            header: header.clone(),
            rows,
            reveals,
            tree,
            anchor: None,
        });
        Some(header)
    }

    /// Every key book a header names, for publication.
    pub fn books(&self) -> &[KeyBook] {
        &self.books
    }

    /// Record where batch `post.seq` landed (from the finalized record).
    pub fn anchor(&mut self, post: &Post) -> Result<(), IntakeError> {
        let b = usize::try_from(post.seq)
            .ok()
            .and_then(|i| self.batches.get_mut(i))
            .ok_or(IntakeError::HiddenPost)?;
        if b.header.hash() != post.header_hash || b.anchor.is_some_and(|a| a != post.anchor) {
            return Err(IntakeError::PostMismatch);
        }
        b.anchor = Some(post.anchor);
        Ok(())
    }

    /// Read the finalized head and every post at or below it, and anchor
    /// the batches. A post the intake did not seal is `HiddenPost`.
    pub fn sync(&mut self, record: &dyn FinalizedRecord) -> Result<Anchor, IntakeError> {
        let head = record.head().map_err(IntakeError::Record)?;
        let posts = record.posts(&head).map_err(IntakeError::Record)?;
        for (i, p) in posts.iter().enumerate() {
            if p.seq != i as u64 {
                return Err(IntakeError::BadChain("post order"));
            }
            self.anchor(p)?;
        }
        Ok(head)
    }

    fn visible<'a>(&'a self, head: &'a Anchor) -> impl Iterator<Item = (&'a Sealed, Anchor)> {
        self.batches
            .iter()
            .filter_map(move |b| b.anchor.filter(|a| a.block <= head.block).map(|a| (b, a)))
    }

    /// The track log at `head` (R-TI-5, R-TI-6) when no level-3 book holds
    /// the keys' statuses: [`Intake::log_with`] with every key active.
    pub fn log_at(&self, head: &Anchor) -> Vec<Statement> {
        let lives: Vec<KeyLives> = self
            .visible(head)
            .filter_map(|(b, a)| {
                self.books
                    .iter()
                    .find(|k| k.hash() == b.header.keys)
                    .map(|k| KeyLives::all_active(k, a.block))
            })
            .collect();
        self.log_with(head, &lives).unwrap_or_default()
    }

    /// The track log at `head` (R-TI-5, R-TI-6, R-TI-9): every statement of
    /// a post at or below the head whose reveal `max(post time, close)` is at
    /// or before the head's time and whose key was active in the level-3 book
    /// at its post's block (`lives`, one per post), committed at its post's
    /// time, canonical. `None` when `lives` is not one list per post covering
    /// that post's book.
    pub fn log_with(&self, head: &Anchor, lives: &[KeyLives]) -> Option<Vec<Statement>> {
        let mut out = Vec::new();
        let mut n = 0;
        for (b, a) in self.visible(head) {
            let book = self.books.iter().find(|k| k.hash() == b.header.keys)?;
            let l = lives.get(n).filter(|l| l.covers(book, a.block))?;
            n += 1;
            for (s, reveal) in b.rows.iter().zip(&b.reveals) {
                if a.timestamp.max(*reveal) <= head.timestamp && l.active(&s.payload.key) {
                    out.push(s.payload.statement(a.timestamp));
                }
            }
        }
        if n != lives.len() {
            return None;
        }
        sort_statements(&mut out);
        Some(out)
    }

    /// The batches as published at `head`: headers of every post at or below
    /// it and the statements revealed by then, with proofs.
    pub fn revealed_at(&self, head: &Anchor) -> Vec<RevealedBatch> {
        self.visible(head)
            .map(|(b, a)| RevealedBatch {
                header: b.header.clone(),
                statements: b
                    .rows
                    .iter()
                    .zip(&b.reveals)
                    .enumerate()
                    .filter(|(_, (_, c))| a.timestamp.max(**c) <= head.timestamp)
                    .filter_map(|(i, (s, _))| b.tree.proof(i as u64).map(|p| (s.clone(), p)))
                    .collect(),
            })
            .collect()
    }
}

/// The intake as published beside a commitment (ROADMAP C20, served by
/// `GET /v1/track/log`): what [`IntakeView`] needs except what a client must
/// bring itself, the level-3 registry's reviewed keys and, in chain mode, the
/// key statuses it reads from the signed-source book. The head and posts are
/// the server's word until the client compares them with its own read of the
/// finalized record ([`FinalizedRecord`]).
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct PublishedIntake {
    /// [`IntakeId::id`].
    pub intake: Hash32,
    /// The finalized head the commitment is at (`as_of` = its timestamp).
    pub head: Anchor,
    /// Every post at or below it, in order.
    pub posts: Vec<Post>,
    /// One revealed batch per post, in order.
    pub batches: Vec<RevealedBatch>,
    /// Every key book a header names.
    pub books: Vec<KeyBook>,
}

impl PublishedIntake {
    /// The view [`verify_log`] checks, with the client's own `reviewed` keys
    /// and key statuses `lives` (one per post; required in chain mode).
    pub fn view<'a>(
        &'a self,
        reviewed: &'a [SourceKey],
        lives: Option<&'a [KeyLives]>,
    ) -> IntakeView<'a> {
        IntakeView {
            intake: self.intake,
            head: &self.head,
            posts: &self.posts,
            batches: &self.batches,
            books: &self.books,
            reviewed,
            lives,
        }
    }

    /// Revealed statements over every batch (the size a server caps).
    pub fn revealed_rows(&self) -> u64 {
        self.batches.iter().map(|b| b.statements.len() as u64).sum()
    }
}

impl Intake {
    /// What the intake publishes at `head` beside a commitment at
    /// `head.timestamp` (ROADMAP C20): every anchored post at or below the
    /// head, its batch with the statements revealed by then, and the key
    /// books. Call [`Intake::sync`] first so every post is anchored.
    pub fn published(&self, head: &Anchor) -> PublishedIntake {
        PublishedIntake {
            intake: self.id,
            head: *head,
            posts: self
                .visible(head)
                .map(|(b, a)| Post {
                    seq: b.header.seq,
                    header_hash: b.header.hash(),
                    anchor: a,
                })
                .collect(),
            batches: self.revealed_at(head),
            books: self.books.clone(),
        }
    }
}

/// What a client reads to check a log: the head and posts from its own view
/// of the record, the published batches and key books.
#[derive(Clone, Copy, Debug)]
pub struct IntakeView<'a> {
    /// [`IntakeId::id`].
    pub intake: Hash32,
    /// The finalized head the commitment is at.
    pub head: &'a Anchor,
    /// Every post at or below it ([`FinalizedRecord::posts`]).
    pub posts: &'a [Post],
    /// One revealed batch per post, in order.
    pub batches: &'a [RevealedBatch],
    /// The key books the headers name.
    pub books: &'a [KeyBook],
    /// The level-3 registry's reviewed keys (every key of every book must be
    /// one, TR-R3-2).
    pub reviewed: &'a [SourceKey],
    /// One [`KeyLives`] per post: the level-3 status of its book's keys at
    /// its block, read by the client from the signed-source book
    /// (`onyx_coordinator::track_chain::CastFacts::key_lives_at`; TR-R4-4).
    /// `None` only when `params.chain` is zero (no level-3 book on chain: the
    /// coordinator's `revoked` flags stand, trusted); with a chain pinned it is
    /// required (`BadKeys`).
    pub lives: Option<&'a [KeyLives]>,
}

/// Check that `statements` (a commitment's log at `as_of`) is exactly what the
/// intake may have revealed by the head (R-TI-8).
pub fn verify_log(
    v: &IntakeView<'_>,
    params: &TrackParams,
    reg: &dyn Registry,
    as_of: u64,
    statements: &[Statement],
) -> Result<(), IntakeError> {
    if as_of != v.head.timestamp {
        return Err(IntakeError::NotHead);
    }
    // The params pin the intake for the whole commitment chain (TR-R3-3).
    if params.intake != v.intake {
        return Err(IntakeError::WrongIntake);
    }
    if v.posts.len() != v.batches.len() {
        return Err(IntakeError::HiddenPost);
    }
    // TR-R4-4: with a level-3 book on chain, its statuses decide, never the
    // coordinator's flags.
    if params.chain != Hash32::ZERO && v.lives.is_none() {
        return Err(IntakeError::BadKeys("level-3 key statuses required"));
    }
    if v.lives.is_some_and(|l| l.len() != v.posts.len()) {
        return Err(IntakeError::BadKeys("one key status list per post"));
    }
    let mut prev = Hash32::ZERO;
    let mut prev_book: Option<&KeyBook> = None;
    let mut last: Option<Anchor> = None;
    let mut seen = BTreeSet::new();
    let mut expected = Vec::new();
    for (i, (post, b)) in v.posts.iter().zip(v.batches).enumerate() {
        let a = post.anchor;
        if post.seq != i as u64
            || a.block > v.head.block
            || a.timestamp > v.head.timestamp
            || last.is_some_and(|l| a.block < l.block || a.timestamp < l.timestamp)
        {
            return Err(IntakeError::BadChain("post order"));
        }
        last = Some(a);
        let h = &b.header;
        if h.hash() != post.header_hash {
            return Err(IntakeError::PostMismatch);
        }
        if h.intake != v.intake || h.seq != i as u64 || h.prev != prev {
            return Err(IntakeError::BadChain("header"));
        }
        let book = v
            .books
            .iter()
            .find(|k| k.hash() == h.keys)
            .ok_or(IntakeError::BadKeys("book not published"))?;
        book.validate(reg, v.reviewed)?;
        if prev_book.is_some_and(|p| !book.extends(p)) {
            return Err(IntakeError::KeysNotForward);
        }
        let lives = match v.lives {
            Some(l) if !l[i].covers(book, a.block) => {
                return Err(IntakeError::BadKeys(
                    "key statuses are not the book's at the post",
                ));
            }
            Some(l) => Some(&l[i]),
            None => None,
        };
        // Review TR-C9-R1-8: in chain mode the coordinator's flag cannot
        // censor either: a key the level-3 book shows active at the post may
        // not be marked revoked (revocation is for keys the book retired or
        // compromised, never for a suspension, which can be lifted).
        if lives.is_some_and(|l| {
            book.keys
                .iter()
                .zip(&l.keys)
                .any(|(k, (_, life))| k.revoked && *life == KeyLife::Active)
        }) {
            return Err(IntakeError::BadKeys(
                "revoked key active in the level-3 book",
            ));
        }
        let mut indices = BTreeSet::new();
        for (s, proof) in &b.statements {
            if proof.leaf_count != h.statement_count
                || !indices.insert(proof.index)
                || !onyx_merkle::verify(&h.statements_root, &s.row(), proof)
            {
                return Err(IntakeError::BadProof);
            }
            if s.payload.intake != v.intake {
                return Err(IntakeError::WrongIntake);
            }
            book.check(s)?;
            if !seen.insert(s.payload.digest()) {
                return Err(IntakeError::Replay);
            }
            let st = s.payload.statement(a.timestamp);
            admissible(params, reg, &st)?;
            let reveal = params
                .reveal_of(&st.fact)
                .ok_or(IntakeError::Track(TrackError::BadPeriod))?;
            // One reveal time per batch (TR-R3-1): a far `reveal_by` cannot
            // keep a statement sealed past its own reveal.
            if reveal != h.reveal_by {
                return Err(IntakeError::BadChain("reveal_by"));
            }
            if a.timestamp.max(reveal) > as_of {
                return Err(IntakeError::EarlyReveal);
            }
            // R-TI-9: revealed and proven, but it counts only if its key was
            // active in the level-3 book at the post's block.
            if lives.is_none_or(|l| l.active(&s.payload.key)) {
                expected.push(st);
            }
        }
        if a.timestamp.max(h.reveal_by) <= as_of && b.statements.len() as u64 != h.statement_count {
            return Err(IntakeError::Incomplete);
        }
        prev = h.hash();
        prev_book = Some(book);
    }
    sort_statements(&mut expected);
    if expected != statements {
        return Err(IntakeError::LogMismatch);
    }
    Ok(())
}

/// Why a receipt cannot be checked.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ReceiptError {
    /// Not for this intake, or not for this statement.
    #[error("receipt is for another intake or statement")]
    WrongReceipt,
    /// The signature does not recover to the intake's poster.
    #[error("receipt not signed by the intake's poster")]
    BadSignature,
    /// The promised deadline is after the statement's close (a receipt that
    /// promises nothing useful), or the statement is not admissible.
    #[error("receipt deadline after the close, or statement not admissible")]
    BadDeadline,
    /// The inclusion proof or the batches do not match the posts.
    #[error("inclusion does not match the posted batch")]
    BadInclusion,
}

/// Check what a receipt shows at the view's finalized head (R-TI-10): signed
/// by `id.poster` for `statement`, deadline at or before the statement's
/// close; then where the statement is. The view's posts and batches are every
/// post at or below the head and the published batches (one per post; the
/// caller checks them with [`verify_log`], which makes every batch past its
/// reveal complete), and `inclusion` the intake's proof for a still-sealed row
/// ([`Intake::inclusion`]): `Kept` when a post at or before the deadline
/// carries it, `PostedLate` when a later one does, `Broken` when none shows it
/// once the head is past the deadline and the close (an on-time post would be
/// revealed by then), else `Pending`. Evidence only: no commit time or outcome
/// changes.
pub fn check_receipt(
    id: &IntakeId,
    v: &IntakeView<'_>,
    params: &TrackParams,
    reg: &dyn Registry,
    r: &SignedReceipt,
    statement: &SignedStatement,
    inclusion: Option<(u64, &Proof)>,
) -> Result<ReceiptStatus, ReceiptError> {
    let (head, posts, batches) = (v.head, v.posts, v.batches);
    if v.intake != id.id()
        || r.receipt.intake != id.id()
        || statement.payload.intake != id.id()
        || r.receipt.digest != statement.payload.digest()
    {
        return Err(ReceiptError::WrongReceipt);
    }
    if eip191_recover(r.receipt.message().as_bytes(), &r.signature) != Some(id.poster) {
        return Err(ReceiptError::BadSignature);
    }
    let close = admissible(params, reg, &statement.payload.statement(0))
        .map_err(|_| ReceiptError::BadDeadline)?;
    let reveal = params
        .reveal_of(&statement.payload.fact)
        .ok_or(ReceiptError::BadDeadline)?;
    if r.receipt.deadline > close {
        return Err(ReceiptError::BadDeadline);
    }
    if posts.len() != batches.len() {
        return Err(ReceiptError::BadInclusion);
    }
    let row = statement.row();
    let status = |p: &Post| {
        if p.anchor.timestamp <= r.receipt.deadline {
            ReceiptStatus::Kept {
                seq: p.seq,
                committed_at: p.anchor.timestamp,
            }
        } else {
            ReceiptStatus::PostedLate {
                seq: p.seq,
                committed_at: p.anchor.timestamp,
            }
        }
    };
    let mut found: Option<ReceiptStatus> = None;
    for (p, b) in posts.iter().zip(batches) {
        if b.header.hash() != p.header_hash || p.anchor.block > head.block {
            return Err(ReceiptError::BadInclusion);
        }
        // By payload digest (unique per intake, R-TI-2), not the whole row:
        // the receipt binds the payload, and a second signature over it must
        // not make a kept receipt look broken (review TR-C9-R1-3).
        let shown = b.statements.iter().any(|(s, proof)| {
            s.payload.digest() == r.receipt.digest
                && proof.leaf_count == b.header.statement_count
                && onyx_merkle::verify(&b.header.statements_root, &s.row(), proof)
        });
        let proven = inclusion.is_some_and(|(seq, proof)| {
            seq == p.seq
                && proof.leaf_count == b.header.statement_count
                && onyx_merkle::verify(&b.header.statements_root, &row, proof)
        });
        if (shown || proven) && found.is_none() {
            found = Some(status(p));
        }
    }
    if let Some(f) = found {
        return Ok(f);
    }
    if inclusion.is_some() {
        return Err(ReceiptError::BadInclusion);
    }
    Ok(
        if head.timestamp > r.receipt.deadline && head.timestamp >= reveal {
            ReceiptStatus::Broken
        } else {
            ReceiptStatus::Pending
        },
    )
}

#[cfg(test)]
#[path = "intake_tests.rs"]
mod tests;
