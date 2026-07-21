//! Reserved for the canonical writer-ledger adapter introduced after S1.
//!
//! The borrowed compatibility facade intentionally exposes no writes: it
//! cannot atomically persist runtime receipts alongside legacy graph commits.
