//! Daemon-owned, bounded Git projections used by health reads.

mod projection;

pub(crate) use projection::{
    GitHealthProjectionError, GitHealthProjectionProgressV1, GitHealthProjectionStoreV1,
};
