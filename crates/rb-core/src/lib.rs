//! # rb-core
//!
//! Core engine library for RustBackup: backup job domain, local DuckDB
//! catalog, and (in later milestones) chunking, encryption, repository and
//! restore engines.
//!
//! Library code returns [`Result`] everywhere; panics are forbidden outside
//! tests.

#![deny(missing_docs)]
#![deny(clippy::unwrap_used)]
#![deny(clippy::expect_used)]
// Tests are allowed to panic loudly (rust-best-practices: no unwrap/expect
// outside tests).
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod backend;
pub mod backup;
pub mod catalog;
pub mod chunker;
pub mod compress;
pub mod crypto;
pub mod domain;
pub mod error;
pub mod hash;
pub mod manifest;
pub mod repo;

pub use error::{Error, Result};
