//! Web-safe Mutex and RwLock extensions for WASM environments.
//!
//! The browser's main thread cannot be blocked, so we need to use spin locks
//! instead of regular blocking locks on the main thread.
//!
//! On non-WASM platforms, this falls back to normal blocking locks.

use std::sync::{
    LockResult, Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard,
    TryLockError,
};

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsCast;
#[cfg(target_arch = "wasm32")]
use web_sys::Window;

#[cfg(target_arch = "wasm32")]
thread_local! {
    /// Cached result of whether we're on the main thread.
    static IS_MAIN_THREAD: bool = js_sys::global().dyn_into::<Window>().is_ok();
}

/// Returns whether the current thread is the main thread (Window context).
/// Result is cached in a thread-local for performance.
///
/// On non-WASM platforms, always returns `false` (uses blocking locks).
#[cfg(target_arch = "wasm32")]
pub fn is_main_thread() -> bool {
    IS_MAIN_THREAD.with(|&v| v)
}

/// Returns whether the current thread is the main thread.
/// On non-WASM platforms, always returns `false` to use blocking locks.
#[cfg(not(target_arch = "wasm32"))]
pub fn is_main_thread() -> bool {
    false
}

pub fn is_worker_thread_cached() -> bool {
    !is_main_thread()
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
        if is_main_thread() {
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

/// Extension trait for `std::sync::RwLock` that provides web-safe locking.
pub trait RwLockWebExt<T> {
    /// Acquires a read lock in a web-safe manner.
    ///
    /// On the main thread: Uses spin lock (non-blocking) since the browser
    /// doesn't allow blocking the main thread.
    ///
    /// On worker threads: Uses regular blocking lock.
    fn web_safe_read(&self) -> LockResult<RwLockReadGuard<'_, T>>;

    /// Acquires a write lock in a web-safe manner.
    ///
    /// On the main thread: Uses spin lock (non-blocking) since the browser
    /// doesn't allow blocking the main thread.
    ///
    /// On worker threads: Uses regular blocking lock.
    fn web_safe_write(&self) -> LockResult<RwLockWriteGuard<'_, T>>;
}

impl<T> RwLockWebExt<T> for RwLock<T> {
    fn web_safe_read(&self) -> LockResult<RwLockReadGuard<'_, T>> {
        if is_main_thread() {
            // Spin lock on main thread
            loop {
                match self.try_read() {
                    Ok(guard) => return Ok(guard),
                    Err(TryLockError::WouldBlock) => {
                        std::hint::spin_loop();
                        continue;
                    }
                    Err(TryLockError::Poisoned(e)) => {
                        return Err(PoisonError::new(e.into_inner()));
                    }
                }
            }
        } else {
            self.read()
        }
    }

    fn web_safe_write(&self) -> LockResult<RwLockWriteGuard<'_, T>> {
        if is_main_thread() {
            // Spin lock on main thread
            loop {
                match self.try_write() {
                    Ok(guard) => return Ok(guard),
                    Err(TryLockError::WouldBlock) => {
                        std::hint::spin_loop();
                        continue;
                    }
                    Err(TryLockError::Poisoned(e)) => {
                        return Err(PoisonError::new(e.into_inner()));
                    }
                }
            }
        } else {
            self.write()
        }
    }
}
