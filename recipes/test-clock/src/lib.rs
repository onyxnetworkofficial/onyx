//! Negative test recipe: imports a host clock. The host must reject it at load time.

#![cfg_attr(target_arch = "wasm32", no_std)]

extern crate alloc;

use onyx_recipe_sdk::{OutputRow, RecipeInput, RecipeOutput};

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "env")]
extern "C" {
    fn clock_now() -> u64;
}

#[cfg(target_arch = "wasm32")]
#[allow(unsafe_code)]
fn now() -> u64 {
    // SAFETY: declared import; never actually instantiated because the host rejects imports.
    unsafe { clock_now() }
}

#[cfg(not(target_arch = "wasm32"))]
fn now() -> u64 {
    0
}

/// Returns the current time (forbidden: nondeterministic).
pub fn run(_input: RecipeInput) -> Result<RecipeOutput, u32> {
    Ok(RecipeOutput {
        rows: alloc::vec![OutputRow {
            key: alloc::vec![0],
            value: now().to_le_bytes().to_vec(),
        }],
    })
}

#[cfg(target_arch = "wasm32")]
onyx_recipe_sdk::recipe!(run);
