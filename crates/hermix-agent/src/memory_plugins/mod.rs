//! Bundled memory provider plugin — ByteRover only.
//!
//! Why only ByteRover? On 2026-06-21 the master reviewed the 8
//! bundled plugins (Hindsight, Honcho, Mem0, RetainDB, ByteRover,
//! OpenViking, SuperMemory, Holographic) against the write-path
//! contract and culled all but ByteRover. On 2026-06-26 the
//! `MemoryProviderPlugin` trait was removed entirely — all write
//! hooks are now routed through `MemoryRouter` (which fans out to
//! every `MemoryLayer` backend).
//!
//! ByteRover exposes a `MemoryLayer` impl (both read + write hooks)
//! so it can be registered as a backend in the MemoryRouter. If the
//! `brv` CLI is not on PATH, `is_available()` returns false and the
//! MemoryLayer impl is a no-op.

pub mod byterover;

#[cfg(test)]
mod tests {
    //! ByteRover is wired unconditionally into the MemoryRouter via
    //! `memory_layer_adapter::all_plugins_as_router`. Its
    //! `is_available()` gates on the `brv` CLI being on PATH — when
    //! missing, the backend is a safe no-op.

    #[test]
    fn byterover_is_a_memory_layer() {
        // Compile-time assertion: ByteRoverPlugin implements MemoryLayer
        use crate::memory_plugins::byterover::ByteRoverPlugin;
        use hermix_focus::MemoryLayer;
        fn _assert_trait_object(_: &dyn MemoryLayer) {}
        let plugin = ByteRoverPlugin::new();
        _assert_trait_object(&plugin);
    }
}