//! The publisher application (ADR-0066 items 5, 6, 8, 9 and 12): one signed
//! file a would-be publisher puts on its origin, and every check of it that
//! needs no person and no network.
//!
//! - [`Application`]: the fields, their JSON form (at most
//!   [`MAX_APPLICATION_BYTES`], unknown fields refused, no contact field) and
//!   the signature: EIP-191 over `onyx-publisher-application-v1:0x<digest>`,
//!   the digest being `keccak256(domain ‖ borsh(fields))`. The domain is not
//!   `onyx-signed-source-v1`, so an application is never a book report.
//! - [`Application::verify`]: the signature, the network (chain id and book),
//!   the expiry, the keys document, the id and origin rules against the keys
//!   the book holds, the cosignature of a second key, and the records of
//!   refused and expired applications.
//! - [`Application::check_sample`]: the first dataset, with the same checks
//!   a node and a reader run (manifest, shard, rights, signed report).
//! - [`Request`]: what a registered publisher asks for later, signed the
//!   same way under its own domain.
//!
//! Nothing here reads a clock, a file or a socket: the caller hands in the
//! time and the bytes.

use crate::ids::{check_origin, check_publisher_id, look_alike, IdError, Origin, OriginError};
use crate::manifest::{Manifest, RIGHTS_STATEMENT};
use crate::shard::check_shard;
use borsh::{BorshDeserialize, BorshSerialize};
use onyx_core::ids::is_dotted_id;
use onyx_core::{keccak256, Hash32, Redistribution, SignedReport};
use onyx_intake::submission::has_email;
use onyx_labels::attest::{eip191_recover, eip191_sign_with};
use serde_json::Value;

/// Domain of an application's digest and signed text.
pub const APPLICATION_DOMAIN: &str = "onyx-publisher-application-v1";
/// Domain of a later request's digest and signed text.
pub const REQUEST_DOMAIN: &str = "onyx-publisher-request-v1";
/// The application's file name, in `.well-known/` of the origin.
pub const APPLICATION_PATH: &str = ".well-known/onyx-publisher-application.json";
/// A later request's file name, in `.well-known/` of the origin.
pub const REQUEST_PATH: &str = ".well-known/onyx-publisher-request.json";
/// The format version.
pub const VERSION: u8 = 1;
/// Largest application or request file.
pub const MAX_APPLICATION_BYTES: usize = 4096;
/// Largest keys document.
pub const MAX_KEYS_DOCUMENT_BYTES: usize = 16 << 10;
/// Largest `data` text.
pub const MAX_DATA_BYTES: usize = 500;
/// Largest sample path.
pub const MAX_PATH_BYTES: usize = 128;
/// An application lives 6 days.
pub const LIFETIME_SECS: u64 = 6 * 86_400;
/// A first application's sample is built within the hour before it.
pub const SAMPLE_MAX_AGE_SECS: u64 = 3_600;
/// Clock skew allowed for `created_unix`.
pub const SKEW_SECS: u64 = 300;
/// The sample: one shard of at most 8 MiB.
pub const MAX_SAMPLE_BYTES: u64 = 8 << 20;
/// Largest sample manifest (one shard, so small).
pub const MAX_SAMPLE_MANIFEST_BYTES: usize = 256 << 10;
/// Largest signed report file.
pub const MAX_REPORT_BYTES: usize = 4096;
/// The book refuses a report this old (`MAX_REPORT_AGE_SECS`).
pub const REPORT_LIFE_SECS: u64 = 7 * 86_400;
/// Margin after a refused report went stale.
pub const BLOCK_MARGIN_SECS: u64 = 300;
/// What a new publisher gets unless it asks for less.
pub const DEFAULT_QUOTA_GIB: u32 = 1;
/// What a new publisher gets unless it asks for less.
pub const DEFAULT_DATASETS: u32 = 16;
/// Largest `asks.quota_gib` that is well formed.
pub const MAX_ASK_GIB: u32 = 1024;
/// Largest `asks.datasets` (a catalog's leaves).
pub const MAX_ASK_DATASETS: u32 = 50_000;

/// Why an application, a sample or a request is refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    /// The file breaks the format.
    #[error("malformed: {0}")]
    Form(String),
    /// The signature does not recover `signer`.
    #[error("the signature is not the signer's")]
    Signature,
    /// Signed for another chain or book.
    #[error("made for another network: the {0} differs")]
    Network(&'static str),
    /// Past `expires_unix`.
    #[error("expired at unix {0}: apply again with the same key and the same sample folder")]
    Expired(u64),
    /// The times do not fit the rules.
    #[error("{0}")]
    Time(&'static str),
    /// The keys document does not back the application.
    #[error("keys document: {0}")]
    KeysDocument(String),
    /// The id breaks a rule.
    #[error(transparent)]
    Id(#[from] IdError),
    /// The origin breaks a rule.
    #[error(transparent)]
    Origin(#[from] OriginError),
    /// The id, origin or signer is bound already.
    #[error("{0}")]
    Taken(String),
    /// A second key without a counting cosignature.
    #[error("no valid cosignature: {0}")]
    Cosignature(String),
    /// A refused record still blocks this signer.
    #[error("this key is blocked until unix {0}: its refused sample could still be relayed; apply with a new key")]
    Blocked(u64),
    /// The sample fails.
    #[error("sample: {0}")]
    Sample(String),
    /// A request the signer may not make.
    #[error("request: {0}")]
    Request(String),
    /// The list of the book's keys is not usable.
    #[error("the list of registered keys is wrong: {0}")]
    Book(String),
}

impl Refusal {
    /// A short stable code: the decision record's reason code where
    /// ADR-0066 item 9 has one.
    pub fn code(&self) -> &'static str {
        match self {
            Refusal::Form(_) => "malformed",
            Refusal::Signature => "bad-signature",
            Refusal::Network(_) => "wrong-network",
            Refusal::Expired(_) => "expired",
            Refusal::Time(_) => "bad-time",
            Refusal::KeysDocument(_) => "keys-document",
            Refusal::Id(IdError::LookAlike(_)) | Refusal::Taken(_) => "name-taken",
            Refusal::Id(_) => "name-not-neutral",
            Refusal::Origin(_) => "origin-overlap",
            Refusal::Cosignature(_) => "no-cosignature",
            Refusal::Blocked(_) => "signer-blocked",
            Refusal::Sample(_) => "sample-failed",
            Refusal::Request(_) => "bad-request",
            Refusal::Book(_) => "bad-registered-list",
        }
    }
}

fn form(s: impl Into<String>) -> Refusal {
    Refusal::Form(s.into())
}

/// `0x` and lowercase hex.
pub fn hex0x(b: &[u8]) -> String {
    format!("0x{}", hex::encode(b))
}

fn unhex<const N: usize>(v: &Value, name: &str) -> Result<[u8; N], Refusal> {
    let bad = || form(format!("{name} must be 0x and {} hex digits", N * 2));
    let s = v
        .as_str()
        .and_then(|s| s.strip_prefix("0x"))
        .ok_or_else(bad)?;
    if s.bytes().any(|c| c.is_ascii_uppercase()) {
        return Err(bad());
    }
    let b = hex::decode(s).map_err(|_| bad())?;
    <[u8; N]>::try_from(b).map_err(|_| bad())
}

fn text(v: &Value, name: &str) -> Result<String, Refusal> {
    v.as_str()
        .map(str::to_string)
        .ok_or_else(|| form(format!("{name} must be text")))
}

fn number(v: &Value, name: &str) -> Result<u64, Refusal> {
    v.as_u64()
        .ok_or_else(|| form(format!("{name} must be a whole number")))
}

fn small(v: &Value, name: &str) -> Result<u32, Refusal> {
    u32::try_from(number(v, name)?).map_err(|_| form(format!("{name} is too large")))
}

/// The object `v` with exactly the keys `keys` (an unknown or missing field
/// is refused: no field can carry a contact detail unseen).
fn object<'a>(
    v: &'a Value,
    what: &str,
    keys: &[&str],
) -> Result<&'a serde_json::Map<String, Value>, Refusal> {
    let m = v
        .as_object()
        .ok_or_else(|| form(format!("{what} must be an object")))?;
    if let Some(k) = m.keys().find(|k| !keys.contains(&k.as_str())) {
        return Err(form(format!("unknown field {k} in {what}")));
    }
    if let Some(k) = keys.iter().find(|k| !m.contains_key(**k)) {
        return Err(form(format!("{what} lacks {k}")));
    }
    Ok(m)
}

/// JSON in which no object names a field twice. `serde_json` alone keeps the
/// last of two equal keys, so a person reading the first value and the tool
/// checking the last would see two files (review APPLY-R1 L3).
pub fn strict_json(bytes: &[u8]) -> Result<Value, String> {
    use serde::de::{Deserialize, Deserializer, Error, MapAccess, SeqAccess, Visitor};
    struct Strict(Value);
    struct V;
    impl<'de> Visitor<'de> for V {
        type Value = Value;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a JSON value")
        }
        fn visit_bool<E>(self, b: bool) -> Result<Value, E> {
            Ok(Value::Bool(b))
        }
        fn visit_i64<E>(self, n: i64) -> Result<Value, E> {
            Ok(Value::from(n))
        }
        fn visit_u64<E>(self, n: u64) -> Result<Value, E> {
            Ok(Value::from(n))
        }
        fn visit_f64<E>(self, n: f64) -> Result<Value, E> {
            Ok(Value::from(n))
        }
        fn visit_str<E>(self, t: &str) -> Result<Value, E> {
            Ok(Value::String(t.to_string()))
        }
        fn visit_unit<E>(self) -> Result<Value, E> {
            Ok(Value::Null)
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Value, A::Error> {
            let mut out = Vec::new();
            while let Some(Strict(v)) = a.next_element()? {
                out.push(v);
            }
            Ok(Value::Array(out))
        }
        fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<Value, A::Error> {
            let mut out = serde_json::Map::new();
            while let Some(k) = a.next_key::<String>()? {
                let Strict(v) = a.next_value()?;
                if out.insert(k.clone(), v).is_some() {
                    return Err(A::Error::custom(format!("the field {k} stands twice")));
                }
            }
            Ok(Value::Object(out))
        }
    }
    impl<'de> Deserialize<'de> for Strict {
        fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            d.deserialize_any(V).map(Strict)
        }
    }
    serde_json::from_slice::<Strict>(bytes)
        .map(|s| s.0)
        .map_err(|e| e.to_string())
}

/// Every entry of the book's list must be in the registered form: an entry
/// spelled another way would hide from the look-alike, taken and overlap
/// checks (review APPLY-R1 M4).
pub fn check_book_list(registered: &[RegisteredKey]) -> Result<(), Refusal> {
    for k in registered {
        if !is_dotted_id(&k.publisher) {
            return Err(Refusal::Book(format!(
                "{:?} is not a publisher id",
                k.publisher
            )));
        }
        if Origin::parse(&k.origin).is_err() {
            return Err(Refusal::Book(format!(
                "the origin {:?} is not in the registered form (lowercase host, optional path, no scheme, no trailing /)",
                k.origin
            )));
        }
    }
    Ok(())
}

fn parse(bytes: &[u8], what: &str) -> Result<Value, Refusal> {
    if bytes.len() > MAX_APPLICATION_BYTES {
        return Err(form(format!(
            "{what} is over {MAX_APPLICATION_BYTES} bytes"
        )));
    }
    strict_json(bytes).map_err(|e| form(format!("{what} is not JSON without repeated fields: {e}")))
}

/// The sample: version 1 of one real dataset, named by a path under the origin.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize)]
pub struct Sample {
    /// Its dataset id.
    pub dataset: String,
    /// Its manifest root.
    pub manifest_root: Hash32,
    /// Its bytes (the manifest's byte count).
    pub bytes: u64,
    /// The version folder's path under the origin, ending in `/`.
    pub path: String,
}

/// The limits the applicant asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize)]
pub struct Asks {
    /// Disk, in GiB.
    pub quota_gib: u32,
    /// Datasets.
    pub datasets: u32,
}

/// Every signed field of an application, in signing order.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize)]
pub struct Fields {
    /// [`VERSION`].
    pub version: u8,
    /// The chain the book is on.
    pub chain_id: u64,
    /// The signed book.
    pub book: [u8; 20],
    /// The publisher id asked for.
    pub publisher: String,
    /// The origin.
    pub origin: String,
    /// The key's address.
    pub signer: [u8; 20],
    /// Keccak of the keys document's exact bytes.
    pub signers_document_keccak: Hash32,
    /// One sentence about the data. Never a name or a contact.
    pub data: String,
    /// The first dataset.
    pub sample: Sample,
    /// The limits asked for.
    pub asks: Asks,
    /// When it was made.
    pub created_unix: u64,
    /// When it lapses: [`LIFETIME_SECS`] later.
    pub expires_unix: u64,
}

/// True for a sample path: parts of lowercase letters, digits, `.`, `-` and
/// `_`, each followed by `/`, none starting with a dot.
pub fn is_sample_path(p: &str) -> bool {
    let part = |s: &str| {
        !s.is_empty()
            && !s.starts_with('.')
            && s.bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"._-".contains(&c))
    };
    p.len() <= MAX_PATH_BYTES && p.strip_suffix('/').is_some_and(|q| q.split('/').all(part))
}

/// The rules of the `data` sentence.
pub fn check_data_text(data: &str) -> Result<(), Refusal> {
    if data.trim().is_empty() || data.len() > MAX_DATA_BYTES {
        return Err(form(format!(
            "data must be one sentence of at most {MAX_DATA_BYTES} bytes"
        )));
    }
    if data.chars().any(char::is_control) {
        return Err(form("data must be one line of plain text"));
    }
    if has_email(data) {
        return Err(form(
            "data must hold no e-mail address or mailto link: an application has no contact field",
        ));
    }
    Ok(())
}

impl Fields {
    /// `keccak256("onyx-publisher-application-v1" ‖ borsh(fields))`.
    pub fn digest(&self) -> Hash32 {
        let mut b = APPLICATION_DOMAIN.as_bytes().to_vec();
        b.extend_from_slice(&borsh::to_vec(self).unwrap_or_default());
        keccak256(&b)
    }

    /// The exact text a key signs: `onyx-publisher-application-v1:0x<digest>`.
    pub fn message(&self) -> String {
        format!("{APPLICATION_DOMAIN}:0x{}", self.digest().to_hex())
    }

    /// The rules a file must keep whoever signed it. The id and origin rules
    /// that depend on the book are in [`Application::verify`].
    pub fn check_form(&self) -> Result<(), Refusal> {
        if self.version != VERSION {
            return Err(form("version must be 1"));
        }
        check_publisher_id(&self.publisher)?;
        check_origin(&self.origin, [])?;
        check_data_text(&self.data)?;
        if !is_dotted_id(&self.sample.dataset) {
            return Err(form("sample.dataset must be a dotted id"));
        }
        if !is_sample_path(&self.sample.path) {
            return Err(form(
                "sample.path must be a folder under the origin, like sample/ (never another host)",
            ));
        }
        if self.sample.bytes == 0 || self.sample.bytes > MAX_SAMPLE_BYTES {
            return Err(form("sample.bytes must be 1 to 8 MiB"));
        }
        let a = self.asks;
        if a.quota_gib == 0
            || a.quota_gib > MAX_ASK_GIB
            || a.datasets == 0
            || a.datasets > MAX_ASK_DATASETS
        {
            return Err(form("asks must be 1 to 1024 GiB and 1 to 50000 datasets"));
        }
        if self.created_unix.checked_add(LIFETIME_SECS) != Some(self.expires_unix) {
            return Err(Refusal::Time(
                "expires_unix must be 6 days after created_unix",
            ));
        }
        Ok(())
    }
}

/// A signed application.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Application {
    /// The signed fields.
    pub fields: Fields,
    /// The signer's signature over [`Fields::message`].
    pub signature: [u8; 65],
    /// For a second key: the same text signed by an active key of the id.
    pub cosignature: Option<[u8; 65]>,
}

/// A key the book holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegisteredKey {
    /// Its publisher id.
    pub publisher: String,
    /// The id's origin.
    pub origin: String,
    /// The key's address.
    pub signer: [u8; 20],
    /// Its life in the book.
    pub life: KeyLife,
}

/// A registered key's life (`keyState`: life and suspension).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyLife {
    /// Counts.
    Active,
    /// Retired by the reviewer.
    Retired,
    /// Marked compromised.
    Compromised,
    /// Suspended after an equivocation.
    Suspended,
}

impl RegisteredKey {
    /// Read the book's keys from a JSON list of
    /// `{"publisher", "origin", "signer", "state"}` (state: `active`,
    /// `retired`, `compromised` or `suspended`).
    pub fn list_from_json(bytes: &[u8]) -> Result<Vec<RegisteredKey>, Refusal> {
        let v = strict_json(bytes)
            .map_err(|e| form(format!("the registered list is not JSON: {e}")))?;
        let list = v
            .as_array()
            .ok_or_else(|| form("the registered list must be a JSON list"))?;
        list.iter()
            .map(|e| {
                let m = object(
                    e,
                    "a registered key",
                    &["publisher", "origin", "signer", "state"],
                )?;
                Ok(RegisteredKey {
                    publisher: text(&m["publisher"], "publisher")?,
                    origin: text(&m["origin"], "origin")?,
                    signer: unhex(&m["signer"], "signer")?,
                    life: match m["state"].as_str() {
                        Some("active") => KeyLife::Active,
                        Some("retired") => KeyLife::Retired,
                        Some("compromised") => KeyLife::Compromised,
                        Some("suspended") => KeyLife::Suspended,
                        _ => {
                            return Err(form(
                                "state must be active, retired, compromised or suspended",
                            ))
                        }
                    },
                })
            })
            .collect::<Result<Vec<_>, Refusal>>()
            .and_then(|l| check_book_list(&l).map(|()| l))
    }
}

/// What a decision record keeps of an application that did not pass
/// (ADR-0066 item 9).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Record {
    /// A refusal: the signer is blocked until its refused report is stale.
    Refused {
        /// The refused key.
        signer: [u8; 20],
        /// [`blocks_signer_until`] of the refused sample.
        blocks_signer_until: u64,
    },
    /// An expiry: a later application of the key must carry the same sample.
    Expired {
        /// The key.
        signer: [u8; 20],
        /// The expired application's sample root.
        manifest_root: Hash32,
    },
}

/// Until when a refused application blocks its signer: the refused sample's
/// creation time plus the report's life and a margin. After it nobody can
/// relay the refused report, whatever is registered.
pub fn blocks_signer_until(sample_created_unix: u64) -> u64 {
    sample_created_unix
        .saturating_add(REPORT_LIFE_SECS)
        .saturating_add(BLOCK_MARGIN_SECS)
}

impl Record {
    /// Read a `decision.json`: `{"state": "refused", "signer",
    /// "blocks_signer_until"}` or `{"state": "expired", "signer",
    /// "sample_manifest_root"}`, other fields ignored. `None` for any other
    /// state (it blocks nothing); an error for a refused or expired record
    /// without its fields.
    pub fn from_json(v: &Value) -> Result<Option<Record>, Refusal> {
        match v["state"].as_str() {
            Some("refused") => Ok(Some(Record::Refused {
                signer: unhex(&v["signer"], "signer")?,
                blocks_signer_until: number(&v["blocks_signer_until"], "blocks_signer_until")?,
            })),
            Some("expired") => Ok(Some(Record::Expired {
                signer: unhex(&v["signer"], "signer")?,
                manifest_root: Hash32(unhex(&v["sample_manifest_root"], "sample_manifest_root")?),
            })),
            Some(_) => Ok(None),
            None => Err(form("a decision record needs a state")),
        }
    }
}

/// A keys document (`onyx-source-keys.json`, ADR-0028): the id and its keys.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeysDocument {
    /// The publisher id it names.
    pub source: String,
    /// The addresses it lists.
    pub signers: Vec<[u8; 20]>,
}

impl KeysDocument {
    /// Parse the document's bytes.
    pub fn parse(bytes: &[u8]) -> Result<KeysDocument, Refusal> {
        let bad = |s: &str| Refusal::KeysDocument(s.to_string());
        if bytes.len() > MAX_KEYS_DOCUMENT_BYTES {
            return Err(bad("over 16 KiB"));
        }
        let v = strict_json(bytes).map_err(|_| bad("not JSON, or a field stands twice"))?;
        let source = v["source"].as_str().ok_or_else(|| bad("no source"))?;
        let keys = v["keys"].as_array().ok_or_else(|| bad("no keys"))?;
        let signers = keys
            .iter()
            .map(|k| {
                unhex::<20>(&k["signer"], "signer")
                    .map_err(|_| bad("a key without a signer address"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(KeysDocument {
            source: source.to_string(),
            signers,
        })
    }
}

/// The network a reviewer reviews for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Network {
    /// Its chain id.
    pub chain_id: u64,
    /// Its signed book.
    pub book: [u8; 20],
}

/// What kind of application passed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Standing {
    /// A new id, a new origin, a new key.
    First,
    /// The same key and sample as an expired application.
    Reapply,
    /// A further key of a registered id, cosigned by this active key.
    SecondKey {
        /// The cosigning key.
        cosigner: [u8; 20],
    },
}

impl Application {
    /// Sign `fields` with `secret`; `fields.signer` must be its address.
    pub fn sign(fields: Fields, secret: &[u8; 32]) -> Result<Application, Refusal> {
        fields.check_form()?;
        let signature =
            eip191_sign_with(secret, fields.message().as_bytes()).ok_or(Refusal::Signature)?;
        let a = Application {
            fields,
            signature,
            cosignature: None,
        };
        if a.recovered() != Some(a.fields.signer) {
            return Err(Refusal::Signature);
        }
        Ok(a)
    }

    /// Add the cosignature of `secret` (an active key of the id).
    pub fn cosign(&mut self, secret: &[u8; 32]) -> Result<(), Refusal> {
        self.cosignature = Some(
            eip191_sign_with(secret, self.fields.message().as_bytes()).ok_or(Refusal::Signature)?,
        );
        Ok(())
    }

    /// Who signed.
    pub fn recovered(&self) -> Option<[u8; 20]> {
        eip191_recover(self.fields.message().as_bytes(), &self.signature)
    }

    /// Who cosigned.
    pub fn cosigner(&self) -> Option<[u8; 20]> {
        let c = self.cosignature.as_ref()?;
        eip191_recover(self.fields.message().as_bytes(), c)
    }

    /// The file's text.
    pub fn to_json(&self) -> String {
        let f = &self.fields;
        let s = |t: &str| Value::String(t.to_string()).to_string();
        let cosig = self
            .cosignature
            .map_or("null".to_string(), |c| s(&hex0x(&c)));
        format!(
            "{{\"version\": {}, \"chain_id\": {}, \"book\": \"{}\",\n \"publisher\": {}, \"origin\": {},\n \"signer\": \"{}\", \"signers_document_keccak\": \"{}\",\n \"data\": {},\n \"sample\": {{\"dataset\": {}, \"manifest_root\": \"{}\", \"bytes\": {},\n            \"path\": {}}},\n \"asks\": {{\"quota_gib\": {}, \"datasets\": {}}},\n \"created_unix\": {}, \"expires_unix\": {},\n \"signature\": \"{}\", \"cosignature\": {}}}\n",
            f.version,
            f.chain_id,
            hex0x(&f.book),
            s(&f.publisher),
            s(&f.origin),
            hex0x(&f.signer),
            hex0x(&f.signers_document_keccak.0),
            s(&f.data),
            s(&f.sample.dataset),
            hex0x(&f.sample.manifest_root.0),
            f.sample.bytes,
            s(&f.sample.path),
            f.asks.quota_gib,
            f.asks.datasets,
            f.created_unix,
            f.expires_unix,
            hex0x(&self.signature),
            cosig,
        )
    }

    /// Read the file: at most [`MAX_APPLICATION_BYTES`], exactly the known
    /// fields, well formed ([`Fields::check_form`]). The signature is not
    /// checked here.
    pub fn from_json(bytes: &[u8]) -> Result<Application, Refusal> {
        let v = parse(bytes, "the application")?;
        let m = object(
            &v,
            "the application",
            &[
                "version",
                "chain_id",
                "book",
                "publisher",
                "origin",
                "signer",
                "signers_document_keccak",
                "data",
                "sample",
                "asks",
                "created_unix",
                "expires_unix",
                "signature",
                "cosignature",
            ],
        )?;
        let sm = object(
            &m["sample"],
            "sample",
            &["dataset", "manifest_root", "bytes", "path"],
        )?;
        let am = object(&m["asks"], "asks", &["quota_gib", "datasets"])?;
        let fields = Fields {
            version: u8::try_from(number(&m["version"], "version")?)
                .map_err(|_| form("version must be 1"))?,
            chain_id: number(&m["chain_id"], "chain_id")?,
            book: unhex(&m["book"], "book")?,
            publisher: text(&m["publisher"], "publisher")?,
            origin: text(&m["origin"], "origin")?,
            signer: unhex(&m["signer"], "signer")?,
            signers_document_keccak: Hash32(unhex(
                &m["signers_document_keccak"],
                "signers_document_keccak",
            )?),
            data: text(&m["data"], "data")?,
            sample: Sample {
                dataset: text(&sm["dataset"], "sample.dataset")?,
                manifest_root: Hash32(unhex(&sm["manifest_root"], "sample.manifest_root")?),
                bytes: number(&sm["bytes"], "sample.bytes")?,
                path: text(&sm["path"], "sample.path")?,
            },
            asks: Asks {
                quota_gib: small(&am["quota_gib"], "asks.quota_gib")?,
                datasets: small(&am["datasets"], "asks.datasets")?,
            },
            created_unix: number(&m["created_unix"], "created_unix")?,
            expires_unix: number(&m["expires_unix"], "expires_unix")?,
        };
        fields.check_form()?;
        Ok(Application {
            fields,
            signature: unhex(&m["signature"], "signature")?,
            cosignature: match &m["cosignature"] {
                Value::Null => None,
                c => Some(unhex(c, "cosignature")?),
            },
        })
    }

    /// Every check that needs no person and no network (ADR-0066 item 8,
    /// steps 1 to 4): the signature, the network, the expiry at `now`, the
    /// keys document's bytes, the id and origin against `registered` (the
    /// book's keys), the cosignature of a second key, and `records` of
    /// refused and expired applications. The sample is
    /// [`Application::check_sample`].
    pub fn verify(
        &self,
        net: &Network,
        now: u64,
        keys_document: &[u8],
        registered: &[RegisteredKey],
        records: &[Record],
    ) -> Result<Standing, Refusal> {
        let f = &self.fields;
        f.check_form()?;
        if self.recovered() != Some(f.signer) {
            return Err(Refusal::Signature);
        }
        check_book_list(registered)?;
        if f.chain_id != net.chain_id {
            return Err(Refusal::Network("chain id"));
        }
        if f.book != net.book {
            return Err(Refusal::Network("book"));
        }
        if now > f.expires_unix {
            return Err(Refusal::Expired(f.expires_unix));
        }
        if f.created_unix > now.saturating_add(SKEW_SECS) {
            return Err(Refusal::Time("created_unix is in the future"));
        }

        if keccak256(keys_document) != f.signers_document_keccak {
            return Err(Refusal::KeysDocument(
                "its bytes do not hash to signers_document_keccak".into(),
            ));
        }
        let doc = KeysDocument::parse(keys_document)?;
        if doc.source != f.publisher {
            return Err(Refusal::KeysDocument(format!(
                "it names {}, not {}",
                doc.source, f.publisher
            )));
        }
        if !doc.signers.contains(&f.signer) {
            return Err(Refusal::KeysDocument("it does not list the signer".into()));
        }
        // The document's hash goes on chain with the registration, so it
        // lists nothing but this id's keys: the signer, and for a further
        // key the keys the book holds for the id (review APPLY-R1 L3).
        let mut seen = Vec::new();
        for k in &doc.signers {
            let of_id = registered
                .iter()
                .any(|r| r.signer == *k && r.publisher == f.publisher);
            if seen.contains(k) || !(*k == f.signer || of_id) {
                return Err(Refusal::KeysDocument(
                    "it must list exactly this application's key (and, for a further key, the keys the book holds for the id), each once".into(),
                ));
            }
            seen.push(*k);
        }

        look_alike(
            &f.publisher,
            &f.origin,
            registered
                .iter()
                .map(|k| (k.publisher.as_str(), k.origin.as_str())),
        )?;
        if registered.iter().any(|k| k.signer == f.signer) {
            return Err(Refusal::Taken(
                "the book already holds this key: a key is registered once".into(),
            ));
        }
        for r in records {
            if let Record::Refused {
                signer,
                blocks_signer_until,
            } = r
            {
                if *signer == f.signer && now < *blocks_signer_until {
                    return Err(Refusal::Blocked(*blocks_signer_until));
                }
            }
        }

        let of_id: Vec<&RegisteredKey> = registered
            .iter()
            .filter(|k| k.publisher == f.publisher)
            .collect();
        if !of_id.is_empty() {
            // The look-alike rule passed, so the origin is the id's: a second
            // key. The origin alone gives nothing (ADR-0066 fact 2).
            let cosigner = self.cosigner().ok_or_else(|| {
                Refusal::Cosignature(
                    "this id is registered: a further key needs the cosignature of an active key of the id".into(),
                )
            })?;
            let key = of_id.iter().find(|k| k.signer == cosigner).ok_or_else(|| {
                Refusal::Cosignature("the cosigner is not a key of this id".into())
            })?;
            if key.life != KeyLife::Active {
                return Err(Refusal::Cosignature(
                    "the cosigning key is retired, compromised or suspended".into(),
                ));
            }
            if !doc.signers.contains(&cosigner) {
                return Err(Refusal::Cosignature(
                    "the keys document must list both keys".into(),
                ));
            }
            return Ok(Standing::SecondKey { cosigner });
        }
        if self.cosignature.is_some() {
            return Err(Refusal::Cosignature(
                "this id is not registered, so nothing can cosign for it".into(),
            ));
        }
        if registered.iter().any(|k| k.origin == f.origin) {
            return Err(Refusal::Taken(
                "the origin belongs to another publisher: one origin, one id".into(),
            ));
        }
        check_origin(&f.origin, registered.iter().map(|k| k.origin.as_str()))?;

        let mut standing = Standing::First;
        for r in records {
            if let Record::Expired {
                signer,
                manifest_root,
            } = r
            {
                if *signer == f.signer {
                    if *manifest_root != f.sample.manifest_root {
                        return Err(Refusal::Sample(
                            "this key's expired application carried another sample: apply again with that same folder".into(),
                        ));
                    }
                    standing = Standing::Reapply;
                }
            }
        }
        Ok(standing)
    }

    /// Check the sample's three files against the application: version 1,
    /// one shard, at most [`MAX_SAMPLE_BYTES`], the shard fits its schema and
    /// hashes into `manifest_root`, the rights are `Allowed` with the fixed
    /// statement, and the report is the signer's (for a second key: the
    /// signer's or the cosigner's). A first application's sample was built
    /// within [`SAMPLE_MAX_AGE_SECS`] before the application.
    pub fn check_sample(
        &self,
        standing: &Standing,
        manifest: &[u8],
        report: &[u8],
        shard: &[u8],
    ) -> Result<SampleFacts, Refusal> {
        let bad = |s: String| Refusal::Sample(s);
        let f = &self.fields;
        if manifest.len() > MAX_SAMPLE_MANIFEST_BYTES || report.len() > MAX_REPORT_BYTES {
            return Err(bad(
                "the manifest or the report is too large for a sample".into()
            ));
        }
        if shard.len() as u64 > MAX_SAMPLE_BYTES {
            return Err(bad("the shard is over 8 MiB".into()));
        }
        let m = Manifest::from_bytes(manifest).map_err(|e| bad(format!("manifest: {e}")))?;
        m.validate().map_err(|e| bad(format!("manifest: {e}")))?;
        let h = &m.header;
        if h.version != 1 {
            return Err(bad(format!(
                "it is version {}: the sample is version 1 of its dataset",
                h.version
            )));
        }
        if h.shard_count != 1 || m.shards.len() != 1 {
            return Err(bad(format!(
                "it has {} shards: the sample is one shard (use fewer rows)",
                h.shard_count
            )));
        }
        if h.publisher.0 != f.publisher || h.dataset != f.sample.dataset {
            return Err(bad("the manifest is of another publisher or dataset".into()));
        }
        if h.root() != f.sample.manifest_root {
            return Err(bad("the manifest does not hash to manifest_root".into()));
        }
        if h.byte_count != f.sample.bytes || h.byte_count > MAX_SAMPLE_BYTES {
            return Err(bad(
                "the manifest's byte count is not sample.bytes, or is over 8 MiB".into(),
            ));
        }
        if h.rights.redistribution != Redistribution::Allowed
            || h.rights_statement != RIGHTS_STATEMENT
        {
            return Err(bad(
                "the rights must be Allowed with the statement onyx-bulk-rights-v1".into(),
            ));
        }
        let c = check_shard(h.schema_of(0), shard).map_err(|e| bad(format!("shard: {e}")))?;
        if c.summary.root != m.shards[0].root
            || c.summary.rows != m.shards[0].rows
            || shard.len() as u64 != m.shards[0].bytes
        {
            return Err(bad("the shard is not the one the manifest lists".into()));
        }
        let r = SignedReport::try_from_slice(report)
            .map_err(|_| bad("report.bin does not decode".into()))?;
        let by = crate::anchor::verify(h, &r).map_err(|e| bad(format!("report: {e}")))?;
        let may_sign = match standing {
            Standing::SecondKey { cosigner } => by == f.signer || by == *cosigner,
            _ => by == f.signer,
        };
        if !may_sign {
            return Err(bad("the report is signed by another key".into()));
        }
        if *standing == Standing::First
            && !(h.created_unix <= f.created_unix
                && f.created_unix - h.created_unix <= SAMPLE_MAX_AGE_SECS)
        {
            return Err(bad(
                "a first application's sample is built within the hour before it: build, sign and apply in one go".into(),
            ));
        }
        Ok(SampleFacts {
            rows: h.row_count,
            bytes: h.byte_count,
            created_unix: h.created_unix,
            stale_unix: h.created_unix.saturating_add(REPORT_LIFE_SECS),
            license: h.rights.license.clone(),
            attribution: h.rights.attribution.clone(),
            columns: h.schema.columns.iter().map(|c| c.name.clone()).collect(),
            report_signer: by,
        })
    }
}

/// What a passed sample shows the reviewer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SampleFacts {
    /// Rows.
    pub rows: u64,
    /// Bytes.
    pub bytes: u64,
    /// When it was built.
    pub created_unix: u64,
    /// When its report can no longer be relayed.
    pub stale_unix: u64,
    /// The licence id.
    pub license: String,
    /// The attribution line.
    pub attribution: Option<String>,
    /// The column names.
    pub columns: Vec<String>,
    /// Who signed the report.
    pub report_signer: [u8; 20],
}

/// A retirement may name a time up to this long before the request without
/// the second proof (clock differences, the time it takes to write it).
pub const RETIRE_WINDOW_SECS: u64 = 3_600;

/// What a registered publisher asks for later (ADR-0066 item 12).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ask {
    /// Withdraw the pending application with this Keccak.
    Withdraw {
        /// Keccak of the application file's bytes.
        application: Hash32,
    },
    /// Raise the limits.
    Raise(Asks),
    /// The signing key was stolen at or after this time.
    KeyStolen {
        /// The theft time.
        since_unix: u64,
    },
    /// Retire a key of the id from this time on.
    RetireKey {
        /// The key to retire.
        key: [u8; 20],
        /// From when.
        at_unix: u64,
    },
}

impl Ask {
    /// Its name in the file.
    pub fn name(&self) -> &'static str {
        match self {
            Ask::Withdraw { .. } => "withdraw",
            Ask::Raise(_) => "raise",
            Ask::KeyStolen { .. } => "key-stolen",
            Ask::RetireKey { .. } => "retire-key",
        }
    }
}

#[derive(BorshSerialize)]
struct RequestSigned<'a> {
    version: u8,
    chain_id: u64,
    book: [u8; 20],
    publisher: &'a str,
    origin: &'a str,
    signer: [u8; 20],
    request: &'a str,
    application: [u8; 32],
    key: [u8; 20],
    at_unix: u64,
    quota_gib: u32,
    datasets: u32,
    sequence: u64,
    created_unix: u64,
    expires_unix: u64,
}

/// A signed later request: the application's shape under [`REQUEST_DOMAIN`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// The chain the book is on.
    pub chain_id: u64,
    /// The signed book.
    pub book: [u8; 20],
    /// The publisher id.
    pub publisher: String,
    /// Its origin.
    pub origin: String,
    /// The signing key's address.
    pub signer: [u8; 20],
    /// What is asked.
    pub ask: Ask,
    /// A number that only grows per signer: a request is taken once, and
    /// none with a sequence at or below the last one taken.
    pub sequence: u64,
    /// When it was made.
    pub created_unix: u64,
    /// When it lapses: [`LIFETIME_SECS`] later.
    pub expires_unix: u64,
    /// The signer's signature over [`Request::message`].
    pub signature: [u8; 65],
}

/// What a request is checked against.
#[derive(Clone, Copy, Debug)]
pub struct RequestContext<'a> {
    /// The keys document at the origin now.
    pub keys_document: &'a [u8],
    /// The book's keys.
    pub registered: &'a [RegisteredKey],
    /// The application at the origin (a `withdraw` names it).
    pub application: Option<&'a [u8]>,
    /// The highest sequence already taken from this signer, if any is known.
    pub last_sequence: Option<u64>,
}

impl Request {
    /// `keccak256("onyx-publisher-request-v1" ‖ borsh(every field but the signature))`.
    pub fn digest(&self) -> Hash32 {
        let (application, key, at_unix, asks) = match self.ask {
            Ask::Withdraw { application } => (application.0, [0; 20], 0, None),
            Ask::Raise(a) => ([0; 32], [0; 20], 0, Some(a)),
            Ask::KeyStolen { since_unix } => ([0; 32], [0; 20], since_unix, None),
            Ask::RetireKey { key, at_unix } => ([0; 32], key, at_unix, None),
        };
        let s = RequestSigned {
            version: VERSION,
            chain_id: self.chain_id,
            book: self.book,
            publisher: &self.publisher,
            origin: &self.origin,
            signer: self.signer,
            request: self.ask.name(),
            application,
            key,
            at_unix,
            quota_gib: asks.map_or(0, |a| a.quota_gib),
            datasets: asks.map_or(0, |a| a.datasets),
            sequence: self.sequence,
            created_unix: self.created_unix,
            expires_unix: self.expires_unix,
        };
        let mut b = REQUEST_DOMAIN.as_bytes().to_vec();
        b.extend_from_slice(&borsh::to_vec(&s).unwrap_or_default());
        keccak256(&b)
    }

    /// The exact text a key signs: `onyx-publisher-request-v1:0x<digest>`.
    pub fn message(&self) -> String {
        format!("{REQUEST_DOMAIN}:0x{}", self.digest().to_hex())
    }

    /// The id and the origin are in their grammar: a request is public, and
    /// its two names are never free text.
    pub fn check_form(&self) -> Result<(), Refusal> {
        if !is_dotted_id(&self.publisher) {
            return Err(form("publisher must be a publisher id"));
        }
        Origin::parse(&self.origin)?;
        if self.created_unix.checked_add(LIFETIME_SECS) != Some(self.expires_unix) {
            return Err(Refusal::Time(
                "expires_unix must be 6 days after created_unix",
            ));
        }
        Ok(())
    }

    /// Sign the request with `secret` (the signature field of `self` is replaced).
    pub fn signed(mut self, secret: &[u8; 32]) -> Result<Request, Refusal> {
        self.check_form()?;
        self.signature =
            eip191_sign_with(secret, self.message().as_bytes()).ok_or(Refusal::Signature)?;
        if eip191_recover(self.message().as_bytes(), &self.signature) != Some(self.signer) {
            return Err(Refusal::Signature);
        }
        Ok(self)
    }

    /// The file's text.
    pub fn to_json(&self) -> String {
        let s = |t: &str| Value::String(t.to_string()).to_string();
        let null = || "null".to_string();
        let (application, key, at, asks) = match self.ask {
            Ask::Withdraw { application } => (s(&hex0x(&application.0)), null(), null(), null()),
            Ask::Raise(a) => (
                null(),
                null(),
                null(),
                format!(
                    "{{\"quota_gib\": {}, \"datasets\": {}}}",
                    a.quota_gib, a.datasets
                ),
            ),
            Ask::KeyStolen { since_unix } => (null(), null(), since_unix.to_string(), null()),
            Ask::RetireKey { key, at_unix } => {
                (null(), s(&hex0x(&key)), at_unix.to_string(), null())
            }
        };
        format!(
            "{{\"version\": {VERSION}, \"chain_id\": {}, \"book\": \"{}\",\n \"publisher\": {}, \"origin\": {},\n \"signer\": \"{}\", \"request\": \"{}\",\n \"application\": {application},\n \"retire\": {key}, \"at_unix\": {at}, \"asks\": {asks},\n \"sequence\": {}, \"created_unix\": {}, \"expires_unix\": {},\n \"signature\": \"{}\"}}\n",
            self.chain_id,
            hex0x(&self.book),
            s(&self.publisher),
            s(&self.origin),
            hex0x(&self.signer),
            self.ask.name(),
            self.sequence,
            self.created_unix,
            self.expires_unix,
            hex0x(&self.signature),
        )
    }

    /// Read the file: the size cap, exactly the known fields, each once, the
    /// id and the origin in their grammar.
    pub fn from_json(bytes: &[u8]) -> Result<Request, Refusal> {
        let v = parse(bytes, "the request")?;
        let m = object(
            &v,
            "the request",
            &[
                "version",
                "chain_id",
                "book",
                "publisher",
                "origin",
                "signer",
                "request",
                "application",
                // The address of the key to retire. Not named "key": secret
                // scanners take a field of that name with a hex value for a
                // credential, and this file is public.
                "retire",
                "at_unix",
                "asks",
                "sequence",
                "created_unix",
                "expires_unix",
                "signature",
            ],
        )?;
        if number(&m["version"], "version")? != u64::from(VERSION) {
            return Err(form("version must be 1"));
        }
        let (app, key, at, asks) = (&m["application"], &m["retire"], &m["at_unix"], &m["asks"]);
        let only = |ok: bool| {
            if ok {
                Ok(())
            } else {
                Err(form(
                    "application, key, at_unix and asks must fit the request and be null otherwise",
                ))
            }
        };
        let ask = match m["request"].as_str() {
            Some("withdraw") => {
                only(key.is_null() && at.is_null() && asks.is_null())?;
                Ask::Withdraw {
                    application: Hash32(unhex(app, "application")?),
                }
            }
            Some("raise") => {
                only(app.is_null() && key.is_null() && at.is_null())?;
                let am = object(asks, "asks", &["quota_gib", "datasets"])?;
                Ask::Raise(Asks {
                    quota_gib: small(&am["quota_gib"], "asks.quota_gib")?,
                    datasets: small(&am["datasets"], "asks.datasets")?,
                })
            }
            Some("key-stolen") => {
                only(app.is_null() && key.is_null() && asks.is_null())?;
                Ask::KeyStolen {
                    since_unix: number(at, "at_unix")?,
                }
            }
            Some("retire-key") => {
                only(app.is_null() && asks.is_null())?;
                Ask::RetireKey {
                    key: unhex(key, "retire")?,
                    at_unix: number(at, "at_unix")?,
                }
            }
            _ => {
                return Err(form(
                    "request must be withdraw, raise, key-stolen or retire-key",
                ))
            }
        };
        let r = Request {
            chain_id: number(&m["chain_id"], "chain_id")?,
            book: unhex(&m["book"], "book")?,
            publisher: text(&m["publisher"], "publisher")?,
            origin: text(&m["origin"], "origin")?,
            signer: unhex(&m["signer"], "signer")?,
            ask,
            sequence: number(&m["sequence"], "sequence")?,
            created_unix: number(&m["created_unix"], "created_unix")?,
            expires_unix: number(&m["expires_unix"], "expires_unix")?,
            signature: unhex(&m["signature"], "signature")?,
        };
        r.check_form()?;
        Ok(r)
    }

    /// Check a request: the grammar, the signature, the network, the expiry,
    /// the sequence, and that the signer may ask this.
    ///
    /// - `withdraw`: it names one application by its Keccak; that file is at
    ///   the origin, is signed by the request's signer and is for the same
    ///   id and origin. So it withdraws that application and nothing else.
    /// - `raise`: an active key of the id.
    /// - `retire-key`: the key itself (unless compromised) or another active
    ///   key of the id; the key is one of the id's. The time is within
    ///   [`RETIRE_WINDOW_SECS`] before the request or later, and not after
    ///   the request lapses. An **earlier** time withdraws what the key
    ///   signed since then, on every node, for good: it needs the second
    ///   proof as a theft does, a keys document at the origin that no longer
    ///   lists the key (review APPLY-R1 H2).
    /// - `key-stolen`: both proofs. The stolen key signs (a thief has the key
    ///   but not the origin), and the keys document at the origin names the
    ///   id and no longer lists that key (whoever took the origin has no
    ///   key). One of the two is never enough for `markCompromised`. A
    ///   retired key cannot be reported stolen: it is out of the keys
    ///   document already, so the second proof would prove nothing.
    ///
    /// A sequence at or below `last_sequence` is refused, so the same file
    /// is not taken twice within its 6 days.
    pub fn verify(&self, net: &Network, now: u64, ctx: &RequestContext) -> Result<(), Refusal> {
        let no = |s: &str| Refusal::Request(s.to_string());
        self.check_form()?;
        if eip191_recover(self.message().as_bytes(), &self.signature) != Some(self.signer) {
            return Err(Refusal::Signature);
        }
        check_book_list(ctx.registered)?;
        if self.chain_id != net.chain_id {
            return Err(Refusal::Network("chain id"));
        }
        if self.book != net.book {
            return Err(Refusal::Network("book"));
        }
        if now > self.expires_unix {
            return Err(Refusal::Expired(self.expires_unix));
        }
        if self.created_unix > now.saturating_add(SKEW_SECS) {
            return Err(Refusal::Time("created_unix is in the future"));
        }
        if ctx.last_sequence.is_some_and(|last| self.sequence <= last) {
            return Err(no(
                "its sequence is not above the last request taken from this key: a request is taken once",
            ));
        }
        let key_of = |a: &[u8; 20]| {
            ctx.registered.iter().find(|k| {
                k.signer == *a && k.publisher == self.publisher && k.origin == self.origin
            })
        };
        let mine = key_of(&self.signer);
        let active = mine.is_some_and(|k| k.life == KeyLife::Active);
        // The second proof: the keys document at the origin names the id and
        // no longer lists `key`.
        let gone_from_document = |key: &[u8; 20]| -> Result<bool, Refusal> {
            let doc = KeysDocument::parse(ctx.keys_document)?;
            if doc.source != self.publisher {
                return Err(Refusal::KeysDocument("it names another id".into()));
            }
            Ok(!doc.signers.contains(key))
        };
        match self.ask {
            Ask::Withdraw { application } => {
                let bytes = ctx.application.ok_or_else(|| {
                    no("a withdraw names an application: the file must be at the origin")
                })?;
                if keccak256(bytes) != application {
                    return Err(no("the application at the origin is not the one it names"));
                }
                let a = Application::from_json(bytes)?;
                if a.recovered() != Some(a.fields.signer) || a.fields.signer != self.signer {
                    return Err(no("only the application's own key may withdraw it"));
                }
                if a.fields.publisher != self.publisher || a.fields.origin != self.origin {
                    return Err(no("the application is for another id or origin"));
                }
                Ok(())
            }
            Ask::Raise(a) => {
                if a.quota_gib == 0
                    || a.quota_gib > MAX_ASK_GIB
                    || a.datasets == 0
                    || a.datasets > MAX_ASK_DATASETS
                {
                    return Err(no("asks must be 1 to 1024 GiB and 1 to 50000 datasets"));
                }
                active
                    .then_some(())
                    .ok_or_else(|| no("only an active key of the id may ask for a raise"))
            }
            Ask::RetireKey { key, at_unix } => {
                let target =
                    key_of(&key).ok_or_else(|| no("the key to retire is not a key of this id"))?;
                let own = key == self.signer && target.life != KeyLife::Compromised;
                if !(own || active) {
                    return Err(no(
                        "a key is retired by itself or by another active key of the id",
                    ));
                }
                // One signature must not end ANOTHER key for good: whoever
                // holds one key of the id (a thief too) could retire the
                // rest. A foreign key needs the second proof, as a theft
                // does (review S3-R1 L3).
                if key != self.signer && !gone_from_document(&key)? {
                    return Err(no(
                        "another key of the id is retired only with the second proof: a keys document at the origin that no longer lists it",
                    ));
                }
                if at_unix > self.expires_unix {
                    return Err(no("the retirement time is after the request lapses"));
                }
                if at_unix.saturating_add(RETIRE_WINDOW_SECS) < self.created_unix
                    && !gone_from_document(&key)?
                {
                    return Err(no(
                        "an earlier retirement time withdraws what the key signed since then: it needs the second proof, a keys document at the origin that no longer lists the key",
                    ));
                }
                Ok(())
            }
            Ask::KeyStolen { since_unix } => {
                let k = mine.ok_or_else(|| {
                    no("the stolen key must sign, and it is not a key of this id")
                })?;
                if !matches!(k.life, KeyLife::Active | KeyLife::Suspended) {
                    return Err(no(
                        "the key is retired or marked compromised already: nothing is left to report",
                    ));
                }
                if since_unix > now {
                    return Err(no("the theft time is in the future"));
                }
                if !gone_from_document(&self.signer)? {
                    return Err(no(
                        "the keys document at the origin still lists the key: take it out first (the second proof)",
                    ));
                }
                Ok(())
            }
        }
    }
}

#[cfg(test)]
#[path = "application_tests.rs"]
mod tests;
