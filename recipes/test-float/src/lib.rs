//! Negative test recipe: does float math on input. The host must reject it at load time.

#![cfg_attr(target_arch = "wasm32", no_std)]
#![allow(clippy::float_arithmetic)]

extern crate alloc;

use onyx_recipe_sdk::{OutputRow, RecipeInput, RecipeOutput};

/// Averages input bytes in floating point (forbidden).
pub fn run(input: RecipeInput) -> Result<RecipeOutput, u32> {
    let mut sum = 0.0f64;
    for b in &input.source {
        sum += f64::from(*b) * 1.5;
    }
    let v = (sum / (input.source.len() as f64 + 1.0)) as u64;
    Ok(RecipeOutput {
        rows: alloc::vec![OutputRow {
            key: alloc::vec![0],
            value: v.to_le_bytes().to_vec(),
        }],
    })
}

#[cfg(target_arch = "wasm32")]
onyx_recipe_sdk::recipe!(run);
