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

pub mod catalog;
pub mod domain;
pub mod error;

pub use error::{Error, Result};
