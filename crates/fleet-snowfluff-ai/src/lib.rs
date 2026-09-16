//! AI provider abstraction, persona loading, and prompt assembly.
//!
//! This crate depends on nothing project-specific (`fleet-snowfluff-core`
//! or the app crate) so it stays independently testable and so
//! `fleet-snowfluff-core` never has to know AI features exist. The app
//! crate is the only place that integrates this crate with `core`.
