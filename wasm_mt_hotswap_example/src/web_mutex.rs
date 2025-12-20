//! Web-safe Mutex extension for WASM environments.
//!
//! The browser's main thread cannot be blocked, so we need to use spin locks
//! instead of regular blocking locks on the main thread.

use std::sync::{Mutex, MutexGuard, LockResult, PoisonError, TryLockError};
use wasm_bindgen::JsCast;
use web_sys::Window;

thread_local! {
    /// Cached result of whether we're on the main thread.
    /// Lazy-initialized on first access.
    static IS_MAIN_THREAD: bool = js_sys::global().dyn_into::<Window>().is_ok();
}

/// Returns whether the current thread is the main thread (Window context).
/// Result is cached in a thread-local for performance.
pub fn is_main_thread_cached() -> bool {
    IS_MAIN_THREAD.with(|&v| v)
}

/// Returns whether the current thread is a worker thread.
/// Result is cached in a thread-local for performance.
pub fn is_worker_thread_cached() -> bool {
    !is_main_thread_cached()
}

/// Extension trait for `std::sync::Mutex` that provides web-safe locking.
pub trait MutexWebExt<T> {
    /// Acquires the mutex in a web-safe manner.
    ///
    /// On the main thread: Uses spin lock (non-blocking) since the browser
    /// doesn't allow blocking the main thread.
    ///
    /// On worker threads: Uses regular blocking lock.
    ///
    /// It's recommended to always lock briefly to avoid blocking main thread too long time as it makes web page non-responsive.
    fn web_safe_lock(&self) -> LockResult<MutexGuard<'_, T>>;
}

impl<T> MutexWebExt<T> for Mutex<T> {
    fn web_safe_lock(&self) -> LockResult<MutexGuard<'_, T>> {
        if is_main_thread_cached() {
            // Spin lock on main thread. Because main thread cannot block using `i32.wait`
            loop {
                match self.try_lock() {
                    Ok(guard) => return Ok(guard),
                    Err(TryLockError::WouldBlock) => {
                        // it's no-op in wasm now. maybe it will be useful in the future.
                        std::hint::spin_loop();
                        continue;
                    }
                    Err(TryLockError::Poisoned(e)) => {
                        return Err(PoisonError::new(e.into_inner()));
                    }
                }
            }
        } else {
            self.lock()
        }
    }
}
