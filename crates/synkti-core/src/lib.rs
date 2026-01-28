//! Synkti Core - Shared types and traits
//!
//! This crate defines the core abstractions used across:
//! - synkti-agent (node binary)
//! - synkti-cli (user CLI)
//! - synkti-fleet (private, fleet commander)
//!
//! Key types:
//! - SpotProvider trait (interface for cloud providers)
//! - Instance types and health status
//! - Error types

pub mod types;
pub mod traits;
pub mod error;

pub use types::*;
pub use traits::*;
pub use error::*;
