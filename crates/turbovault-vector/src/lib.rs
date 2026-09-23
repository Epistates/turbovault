//! Optional hybrid (dense + lexical) semantic search engine for TurboVault.
//!
//! Host-agnostic: this crate does not depend on `turbovault-core`,
//! `turbovault-vault`, or `turbovault-plugin-api`, and performs no I/O of its
//! own beyond CPU-bound embedding. [`IndexEngine`] is fed note content and
//! hands back what to persist; a host owns reading notes, deciding what
//! changed, and storing the result. `crates/plugins/turbovault-plugin-vector`
//! is that host today, wiring this engine to the TurboVault plugin boundary.
//!
//! See the crate README for the stack this is built on and why, and for
//! credit to the prototype this design started from.

pub mod chunk;
pub mod config;
pub mod dense;
pub mod embedding;
pub mod engine;
pub mod error;
pub mod lexical;
pub mod router;
pub mod store;

pub use chunk::{chunk_text, content_hash, diff_chunks};
pub use config::VectorConfig;
pub use dense::DenseIndex;
pub use embedding::{EmbeddingEngine, Model2VecEmbedder};
pub use engine::{IndexEngine, IndexStats, SearchHit};
pub use error::{Result, VectorError};
pub use lexical::LexicalIndex;
pub use router::{FusedResult, reciprocal_rank_fusion};
pub use store::{ChunkRecord, NoteRecord};
