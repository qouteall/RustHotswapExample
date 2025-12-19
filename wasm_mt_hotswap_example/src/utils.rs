//! Utility functions for wasm multi-threading support.

use std::mem;

/// Decompose a Box<dyn Trait> into its two pointer components (data, vtable).
pub fn decompose_box<T: ?Sized>(b: Box<T>) -> (*mut (), *mut ()) {
    let fat_ptr: *mut T = Box::into_raw(b);
    // Transmute the fat pointer into its raw components: (data, vtable) for trait objects
    unsafe { mem::transmute_copy::<*mut T, (*mut (), *mut ())>(&fat_ptr) }
}

/// Reconstruct a Box<dyn Trait> from its two pointer components.
///
/// # Safety
/// The caller must ensure that:
/// - `data` and `vtable` were obtained from a previous call to `decompose_box`
/// - The original Box has not been reconstructed elsewhere (no double-free)
/// - The type T matches the original type used in `decompose_box`
pub unsafe fn reconstruct_box<T: ?Sized>(data: *mut (), vtable: *mut ()) -> Box<T> {
    // Ensure that *mut T is actually a fat pointer (2 * pointer size)
    assert_eq!(
        mem::size_of::<*mut T>(),
        mem::size_of::<(*mut (), *mut ())>(),
        "reconstruct_box called with a type that is not a 2-pointer fat pointer"
    );

    let parts = (data, vtable);
    let fat_ptr: *mut T = mem::transmute_copy(&parts);
    Box::from_raw(fat_ptr)
}
