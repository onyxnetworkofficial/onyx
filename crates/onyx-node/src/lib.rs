//! The onyx-api service (docs/ops/deployment.md, docs/ops/run-api.md).
//!
//! `serve-api` and the demo drivers build their state in memory from a round
//! they settle themselves. This crate is the long-running server instead: it
//! reads its configuration from the environment ([`config::NodeConfig`]),
//! the deployed contracts from a deployment manifest ([`manifest::Deployment`]),
//! the shards it serves from a catalog file ([`catalog_file::CatalogFile`]),
//! the rows from a content-addressed directory, and the finalized roots from
//! the chain. Payments are either off, settled by the server itself into
//! `OnyxRevenue` (ADR-0026), or sent to an HTTPS x402 facilitator; no mock or
//! dev facilitator exists in this crate.

pub mod app;
pub mod catalog_file;
pub mod config;
pub mod doctor;
pub mod healthcheck;
pub mod manifest;
pub mod observations;
pub mod observe;
pub mod serve;
pub mod shutdown;
pub mod submissions;
