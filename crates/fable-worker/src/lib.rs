//! Sync engine, index orchestration, scratch manager and parse-worker pool.

#![forbid(unsafe_code)]

pub mod scratch;

pub use scratch::{
    ScratchDir, ScratchError, ScratchFile, ScratchManager, ScratchWriter, SweepReport,
};
