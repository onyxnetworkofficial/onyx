//! What recipe authors import.
//!
//! A recipe is a pure function `input bytes -> output bytes`, compiled to
//! `wasm32-unknown-unknown`. The host gives it **no imports at all** in ABI v1:
//! no clock, no randomness, no network, no filesystem. Output rows are
//! Borsh-encoded with integer and fixed-point values only (ADR-0002, ADR-0009).
//! The host rejects any module that contains a float instruction (ADR-0004).
//! The full SDK (allocator, input decoding, `#[recipe]` entry macro) lands in M1.

#![no_std]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use borsh::{BorshDeserialize, BorshSerialize};

/// What the host passes to `onyx_run`, Borsh-encoded. Sector-agnostic: each
/// part is opaque bytes whose schema the recipe knows.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct RecipeInput {
    /// Must equal [`ABI_VERSION`].
    pub abi_version: u32,
    /// Canonical source data from the sector adapter for the pinned snapshot.
    pub source: Vec<u8>,
    /// Borsh of the pinned `onyx_labels::LabelSet`, or empty if the metric needs none.
    /// Decode with [`LabelSetView`].
    pub labels: Vec<u8>,
    /// Shard parameters (`ShardSpec.params`).
    pub params: Vec<u8>,
}

/// Read-only mirror of `onyx_labels::LabelSet` with the same Borsh layout, so
/// recipes can decode a label set without pulling in std crates. A test in
/// `onyx-labels` proves the layouts match.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct LabelSetView {
    /// Label set name.
    pub name: String,
    /// Version.
    pub version: u32,
    /// Labels, strictly ascending by address.
    pub labels: Vec<LabelView>,
}

impl LabelSetView {
    /// Addresses strictly ascending (one label per address). The host already
    /// validated the set; recipes re-check cheaply rather than trust it.
    pub fn is_canonical(&self) -> bool {
        self.labels.windows(2).all(|w| w[0].address < w[1].address)
    }
}

/// Mirror of `onyx_labels::Label`.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct LabelView {
    /// Namespaced address, e.g. `btc:bc1q...`.
    pub address: String,
    /// Entity id.
    pub entity: String,
    /// `EntityKind` discriminant (Borsh enum tag).
    pub kind: u8,
    /// Confidence, ppm.
    pub confidence_ppm: u32,
    /// Evidence `(kind, content_hash)`.
    pub evidence: Vec<(String, [u8; 32])>,
}

/// ABI version recipes and host agree on.
pub const ABI_VERSION: u32 = 1;

/// Export the host calls to reserve `len` bytes for input. Returns a pointer.
pub const EXPORT_ALLOC: &str = "onyx_alloc";

/// Export the host calls to run the recipe: `(ptr, len) -> u64` where the
/// result packs the output pointer (high 32 bits) and length (low 32 bits).
pub const EXPORT_RUN: &str = "onyx_run";

/// Export that returns [`ABI_VERSION`].
pub const EXPORT_ABI_VERSION: &str = "onyx_abi_version";

/// One output row. Rows are strictly ascending by `key`, so keys are unique
/// (R-DUPKEY-1). The Merkle leaf is the Borsh encoding of the whole row.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct OutputRow {
    /// Primary key bytes (e.g. the Borsh of the entity id).
    pub key: Vec<u8>,
    /// Row bytes (Borsh of the row schema).
    pub value: Vec<u8>,
}

/// A recipe's successful result.
#[derive(Clone, Debug, Default, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct RecipeOutput {
    /// Rows, strictly ascending by key.
    pub rows: Vec<OutputRow>,
}

/// What `onyx_run` writes back, Borsh-encoded.
#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum RunReturn {
    /// Rows.
    Ok(RecipeOutput),
    /// Recipe-defined failure code (e.g. inconsistent input). Committed as a canonical failure row.
    Err(u32),
}

/// Pack an output pointer and length into the `onyx_run` return value.
pub const fn pack_ptr_len(ptr: u32, len: u32) -> u64 {
    ((ptr as u64) << 32) | len as u64
}

/// Inverse of [`pack_ptr_len`].
pub const fn unpack_ptr_len(v: u64) -> (u32, u32) {
    ((v >> 32) as u32, v as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_roundtrip() {
        assert_eq!(
            unpack_ptr_len(pack_ptr_len(0xdead_beef, 7)),
            (0xdead_beef, 7)
        );
    }
}

/// Runtime for recipes compiled to `wasm32-unknown-unknown` with the `rt` feature:
/// a deterministic bump allocator, a trapping panic handler and the export glue
/// behind [`recipe!`]. Never used by the host.
#[cfg(all(target_arch = "wasm32", feature = "rt"))]
#[allow(unsafe_code)]
pub mod rt {
    use super::*;
    use core::alloc::{GlobalAlloc, Layout};
    use core::sync::atomic::{AtomicUsize, Ordering};

    const PAGE: usize = 65_536;

    /// Bump allocator: allocates from the end of linear memory upward and never
    /// frees. Fully deterministic; a run is one-shot, so leaking is fine.
    pub struct Bump;

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    // SAFETY: single-threaded wasm32 (threads are rejected by the host); every
    // returned block lies in memory obtained from `memory.grow` that no one else uses.
    unsafe impl GlobalAlloc for Bump {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            let mut next = NEXT.load(Ordering::Relaxed);
            if next == 0 {
                next = core::arch::wasm32::memory_size(0) * PAGE;
            }
            let start = (next + layout.align() - 1) & !(layout.align() - 1);
            let Some(end) = start.checked_add(layout.size()) else {
                return core::ptr::null_mut();
            };
            let have = core::arch::wasm32::memory_size(0) * PAGE;
            if end > have {
                let pages = (end - have).div_ceil(PAGE);
                if core::arch::wasm32::memory_grow(0, pages) == usize::MAX {
                    return core::ptr::null_mut();
                }
            }
            NEXT.store(end, Ordering::Relaxed);
            start as *mut u8
        }

        unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {}
    }

    #[global_allocator]
    static ALLOC: Bump = Bump;

    #[panic_handler]
    fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
        core::arch::wasm32::unreachable()
    }

    /// `onyx_alloc` body: reserve `len` bytes for the host to write input into.
    pub fn alloc(len: u32) -> u32 {
        let v: Vec<u8> = alloc::vec![0u8; len as usize];
        let ptr = v.as_ptr() as u32;
        core::mem::forget(v);
        ptr
    }

    /// `onyx_run` body: decode input, call the recipe, encode and leak the result.
    pub fn run(ptr: u32, len: u32, f: fn(RecipeInput) -> Result<RecipeOutput, u32>) -> u64 {
        // SAFETY: the host wrote exactly `len` bytes at `ptr`, a block from `alloc`.
        let bytes = unsafe { core::slice::from_raw_parts(ptr as *const u8, len as usize) };
        let ret = match borsh::from_slice::<RecipeInput>(bytes) {
            Ok(input) if input.abi_version == ABI_VERSION => match f(input) {
                Ok(out) => RunReturn::Ok(out),
                Err(code) => RunReturn::Err(code),
            },
            _ => RunReturn::Err(u32::MAX),
        };
        let out = borsh::to_vec(&ret).unwrap_or_default();
        let (p, l) = (out.as_ptr() as u32, out.len() as u32);
        core::mem::forget(out);
        pack_ptr_len(p, l)
    }
}

/// Declare the recipe entry point: `onyx_recipe_sdk::recipe!(my_fn);` where
/// `my_fn: fn(RecipeInput) -> Result<RecipeOutput, u32>`.
#[cfg(all(target_arch = "wasm32", feature = "rt"))]
#[macro_export]
macro_rules! recipe {
    ($f:path) => {
        /// ABI version export.
        #[no_mangle]
        pub extern "C" fn onyx_abi_version() -> u32 {
            $crate::ABI_VERSION
        }
        /// Input buffer allocation export.
        #[no_mangle]
        pub extern "C" fn onyx_alloc(len: u32) -> u32 {
            $crate::rt::alloc(len)
        }
        /// Recipe entry export.
        #[no_mangle]
        pub extern "C" fn onyx_run(ptr: u32, len: u32) -> u64 {
            $crate::rt::run(ptr, len, $f)
        }
    };
}
