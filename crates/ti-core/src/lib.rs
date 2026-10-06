//! Lume TI bitmap core. All implementations are original; no external code is copied.
//! Row mutation uses column clear/install; range evaluation uses lexicographic
//! O'Neil/Quass-style bit prefixes, aggregates use slice popcounts and greedy prefixes,
//! and predicates use SQL Kleene three-valued masks. Algorithms are described per module.
#![forbid(unsafe_code)]
mod evaluator;
pub mod reference;
mod rows;
pub use evaluator::*;
pub use rows::*;
pub use ti_contracts::{
    bucket_of, column_id, local_col, shard_key, CmpOp, Predicate, RoaringBitmap, EPOCH,
};
