//! Bounded, sequenced diagnostics event log and its paged wire response.
//!
//! Wire types are generated from `schema/core.go` into `schema_gen.rs`.
pub use crate::schema_gen::{
    DiagnosticsEvent, DiagnosticsEvents, DiagnosticsKind, EVENTS_PAGE, EventDetails,
};
