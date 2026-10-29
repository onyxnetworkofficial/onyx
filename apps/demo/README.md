# Onyx demo site

A single page that walks through the pipeline and reruns the real recipe in the browser.

1. Export the data with the real Onyx code (writes `site/data/`, not committed):
   `cargo run --release -p onyx-source-btc --example export-demo`
2. Serve `site/` with any static server (for example `python3 -m http.server -d apps/demo/site 8080`)
   and open http://localhost:8080. The page fetches `data/demo.json`, `data/recipe.wasm` and the base64
   inputs, runs the recipe with WebAssembly, and checks the source hash, root, challenges, EIP-191
   signature and Merkle proofs itself.

Published copy (private, share from the page's Share menu): <private link>
