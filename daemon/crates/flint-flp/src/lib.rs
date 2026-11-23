//! FL Studio `.flp` binary parser and semantic musical diff engine.

mod diff;
mod parser;

pub use diff::diff_manifests;
pub use parser::{parse_flp_bytes, parse_flp_path};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum FlpError {
    #[error("invalid FLP header magic (expected 'FLhd')")]
    InvalidHeader,
    #[error("invalid FLP data chunk magic (expected 'FLdt')")]
    InvalidDataChunk,
    #[error("unexpected end of FLP data at byte offset {0}")]
    UnexpectedEof(usize),
    #[error("malformed varint length at byte offset {0}")]
    InvalidVarint(usize),
    #[error("no .flp file found inside zip archive")]
    NoFlpInZip,
    #[error("zip archive error: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}
