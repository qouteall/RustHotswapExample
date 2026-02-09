use std::cell::Cell;
use std::sync::atomic::AtomicUsize;

pub static EXTERNAL_DATA: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    pub static EXTERNAL_TLS: Cell<usize> = Cell::new(0);
}