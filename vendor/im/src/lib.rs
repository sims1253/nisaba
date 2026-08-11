//! Shim crate: re-exports [`imbl`] under the `im` name so that downstream
//! crates (loro-internal) that still reference the archived `im` crate
//! transparently use the maintained `imbl` fork. This eliminates the
//! unmaintained `im`, `sized-chunks`, and `bitmaps` advisories.
pub use imbl::*;
