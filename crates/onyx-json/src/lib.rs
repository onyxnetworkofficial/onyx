//! The generic level-2 JSON fetcher (ADR-0053, contributor loop CL3).
//!
//! - [`reader`]: a strict JSON reader (duplicate keys, byte-order marks, lone surrogates and
//!   nesting deeper than 64 refused; numbers kept as their exact text).
//! - [`extract`]: deterministic extraction of one value under a
//!   [`onyx_core::feed_json::JsonDescriptor`], version 1.
//! - [`form`]: the descriptor's JSON form (source manifests, the vectors file).
//! - [`GenericJsonFetcher`]: the one [`Fetcher`] every manifest source uses; its id is the
//!   descriptor's [`JsonDescriptor::fetcher_id`].
//!
//! No I/O and no dependency but `onyx-core`, so the browser verifier, the intake preview and
//! every observer run the same code. The shared vectors are `fixtures/json-fetcher/vectors.json`.

pub mod extract;
pub mod form;
pub mod reader;
pub mod vectors;

pub use extract::{
    extract, mantissa, read, read_at, rfc3339, subject_bucket_start, subject_date, Why,
};
pub use form::{descriptor_from_json, descriptor_from_node, descriptor_to_json, FormError};
pub use reader::{parse, Node, ReadError};

use onyx_core::feed_json::JsonDescriptor;
use onyx_core::{Answer, Fetcher, Hash32, ObservationSpec};

/// Path of the shared vectors, from the repository root.
pub const VECTORS_PATH: &str = "fixtures/json-fetcher/vectors.json";

/// The generic fetcher for one descriptor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GenericJsonFetcher {
    d: JsonDescriptor,
    id: Hash32,
}

impl GenericJsonFetcher {
    /// The fetcher of `d`, refused when `d` is invalid.
    pub fn new(d: JsonDescriptor) -> Result<Self, onyx_core::feed_json::DescriptorError> {
        d.validate()?;
        let id = d.fetcher_id();
        Ok(GenericJsonFetcher { d, id })
    }

    /// Its descriptor.
    pub fn descriptor(&self) -> &JsonDescriptor {
        &self.d
    }
}

impl Fetcher for GenericJsonFetcher {
    fn id(&self) -> Hash32 {
        self.id
    }

    fn extract(&self, spec: &ObservationSpec, page: &[u8]) -> Answer {
        if spec.fetcher != self.id || spec.selector != self.d.pointer {
            return Answer::Unavailable;
        }
        read_at(
            &self.d,
            spec.kind,
            subject_date(&spec.subject),
            subject_bucket_start(&spec.subject),
            page,
        )
        .unwrap_or(Answer::Unavailable)
    }
}
