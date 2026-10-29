//! Negative test recipe: imports WASI `random_get`, as the `getrandom` crate
//! does. The host must reject it at load time.

#![cfg_attr(target_arch = "wasm32", no_std)]

extern crate alloc;

use onyx_recipe_sdk::{OutputRow, RecipeInput, RecipeOutput};

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "wasi_snapshot_preview1")]
extern "C" {
    fn random_get(buf: *mut u8, len: usize) -> u16;
}

#[cfg(target_arch = "wasm32")]
#[allow(unsafe_code)]
fn random() -> [u8; 8] {
    let mut b = [0u8; 8];
    // SAFETY: declared import; never instantiated because the host rejects imports.
    unsafe {
        random_get(b.as_mut_ptr(), b.len());
    }
    b
}

#[cfg(not(target_arch = "wasm32"))]
fn random() -> [u8; 8] {
    [0; 8]
}

/// Returns random bytes (forbidden: nondeterministic).
pub fn run(_input: RecipeInput) -> Result<RecipeOutput, u32> {
    Ok(RecipeOutput {
        rows: alloc::vec![OutputRow {
            key: alloc::vec![0],
            value: random().to_vec(),
        }],
    })
}

#[cfg(target_arch = "wasm32")]
onyx_recipe_sdk::recipe!(run);
