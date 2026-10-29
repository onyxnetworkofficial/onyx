# ADR-0005: Solidity with Foundry on Base Sepolia

## Status
Accepted, 2026-10-06 (M0).

## Context
The settlement layer stores recipe registry entries, commitments, bonds, disputes and finalized roots.

## Decision
- Solidity 0.8.30, Foundry (forge 1.8.5 at M0), `forge-std` pinned as a git submodule (v1.17.0).
- Network: Base Sepolia only. No mainnet deploys (hard limit). Deployer key comes from `.env`, never printed.
- Fuzz tests and invariant tests before any deploy (M6). Shared vectors with Rust for every
  hash or encoding the contract checks.
- No upgradeable proxies in the prototype; a fix is a new deployment with a new address
  recorded in `docs/ops/deployment.md`.

## Alternatives
- Hardhat: fine, but Foundry tests are Solidity and support fuzzing natively.
- An app-chain or rollup of our own: much more operational work; Base gives cheap
  settlement and native USDC for x402.

## Consequences
- Bonds are in testnet ETH in M6; a USDC bond option is an open question.
