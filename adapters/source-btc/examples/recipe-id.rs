//! Print and record the recipe id (Keccak-256) of each wasm file given.
//! Writes `<file>.keccak` next to each file.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    for path in std::env::args().skip(1) {
        let bytes = std::fs::read(&path)?;
        let id = onyx_core::keccak256(&bytes);
        std::fs::write(format!("{path}.keccak"), format!("{id}\n"))?;
        println!("{id}  {path}  ({} bytes)", bytes.len());
    }
    Ok(())
}
