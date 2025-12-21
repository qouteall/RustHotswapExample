# CLAUDE.md - wasm_mt_hotswap_example

Multi-threaded WASM hotswap example. Work-in-progress.

## Modules

- `main.rs` - Entry point with raytracer scene rendering, hotpatch initialization via WebSocket to devserver, and WASM module loading/relocation logic
- `web_worker_manager.rs` - Manages web workers with message passing between threads. Handles WASM module/memory transfer, MessageChannel setup for worker-to-worker communication, and thread IDs. Important.
- `web_mutex.rs` - Web-safe Mutex and RwLock that use spin locks on main thread (since browser main thread can't block) and normal locks on workers. Falls back to normal locking on non-WASM
- `pool.rs` - Legacy worker pool from wasm-bindgen raytrace example. To be replaced
- `utils.rs` - Fat pointer decomposition/reconstruction for sending `Box<dyn FnOnce()>` across threads via raw pointers

## Toolchain

Uses nightly with `-Zbuild-std` and `+atomics,+bulk-memory,+mutable-globals` target features.
