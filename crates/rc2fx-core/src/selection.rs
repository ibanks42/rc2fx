//! Explicit topology requests; effects saves never carry this request.
//! Wire types are generated from `schema/core.go` into `schema_gen.rs`.
pub use crate::schema_gen::{SelectionPhase, SelectionRequest, SelectionStatus};
