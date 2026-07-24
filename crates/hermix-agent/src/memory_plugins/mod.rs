//! Bundled memory provider plugins.
//!
//! All external memory plugins (Hindsight, Honcho, Mem0, RetainDB, ByteRover,
//! OpenViking, SuperMemory, Holographic) have been removed. The project now
//! relies solely on the built-in `EmbeddedMemoryBackend` (SQLite-backed
//! `memory.db`) for persistent memory. The `MemoryProviderPlugin` trait was
//! removed on — all write hooks route through `MemoryRouter`,
//! which fans out to every registered `MemoryLayer` backend.
