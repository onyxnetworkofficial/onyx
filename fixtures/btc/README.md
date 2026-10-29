# Bitcoin block fixtures

`blocks-921080-921082.json`: mainnet blocks 921080, 921081, 921082 with every transaction's
spent prevouts (script, value) and created outputs. Built by `scripts/fetch-btc-fixture.py`
from the public mempool.space Esplora API on 2026-10-06.

Verified at fetch time against the raw blocks: proof of work, prev-hash links across the range,
tip hash confirmed by a second API (blockstream.info), Merkle root with no duplicate txids,
txid order, input counts, outpoints, coinbase derived from the raw tx, outputs, and fee >= 0.
The file stores headers and outpoints; `onyx-source-btc` rechecks header hash, proof of work and
links on every load. Not verified: prevout scripts and values beyond the fee check (asserted by
the API). Recheck against a full node before relying on this outside tests. Rights: public chain data.

| Height | Hash | Txs |
|---|---|---|
| 921080 | 00000000000000000000596bc95e5aee8ff64e2097dd39b15c3c27974595751c | 3868 |
| 921081 | 0000000000000000000093a5abbfc905aca0a9e98ca063de1029206ac6e9106f | 2404 |
| 921082 | 0000000000000000000194cae50bac6b72bca5fc131413d737df8ad10992125a | 5343 |
