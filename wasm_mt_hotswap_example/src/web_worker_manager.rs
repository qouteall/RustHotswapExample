//! Web Worker Manager for managing web workers and message passing.
//!
//! Note: it's work-in-progress
//!
//! ## Background: Web Multi-threading Limitations
//!
//! In-browser WASM multi-threading has restrictions:
//!
//! - Different web workers must use separately-created
//!   `WebAssembly.Instance`. They can only share `WebAssembly.Memory`, not
//!   tables or globals.
//! - Objects like `OffscreenCanvas` and `WebAssembly.Module`
//!   can only be sent via JS `postMessage`, not through `SharedArrayBuffer`.
//! - Web workers cannot receive new JS messages without finishing current JS event processing.
//!   If it keeps running a scheduler loop it cannot receive JS messages.
//! - There is no Web API for getting a list of all
//!   web workers.
//!
//! ## Why Web Worker Manager?
//!
//! This module provides a centralized manager to work around these limitations:
//!
//! - Allocate fixed amount of web workers at initialization,
//! - Handle WASM module/memory transfer and worker initialization.
//! - Allow each threads to send message to each other. Workers use `MessageChannel` to communicate with each other.
//! - Provide an easy way to send Rust closures
//!   combined with JS values between threads.
//! - For dynamic linking (not yet implemented), workers must cooperatively load new WASM modules
//!   and update their own WASM tables.
//!
//! **Important**: Web workers managed by this module must not be touched by raw
//! JS APIs. Don't directly send messages or change their `onmessage` callback.
//!
//! ## Thread IDs
//!
//! - `ThreadId(0)` = main thread
//! - `ThreadId(1..=n)` = worker threads
//!
//! ## Message Protocol
//!
//! Init message (main → worker):
//! ```text
//! {
//!   __wwm_wasm_module: WebAssembly.Module,
//!   __wwm_wasm_memory: WebAssembly.Memory,
//!   __wwm_callback: [dataPtr, vTablePtr],  // fat pointer for Box<dyn FnOnce(ThreadId, JsValue) + Send>
//!   __wwm_js_payload: any,                 // user-defined payload
//!   __wwm_thread_id: number,               // this worker's ThreadId (1..=n)
//!   __wwm_sender_id: number,               // sender's ThreadId (always 0 for init)
//!   __wwm_ports: [MessagePort | null, ...],  // bidirectional ports to other workers
//!   __wwm_thread_count: number             // total number of threads (worker_count + 1)
//! }
//! ```
//!
//! Port array is indexed by ThreadId (index 0 = main thread, 1..=n = workers):
//! - `ports[tid]` is the bidirectional port to communicate with `ThreadId(tid)`
//! - Each `MessageChannel` is shared between two workers (smaller tid gets port1, larger gets port2)
//! - `ports[0]` is always `null` (main thread uses postMessage, not MessageChannel)
//! - `ports[my_tid]` is `null` (no port to self)
//!
//! Task message (any direction - main↔worker or worker↔worker):
//! ```text
//! {
//!   __wwm_callback: [dataPtr, vTablePtr],  // fat pointer for Box<dyn FnOnce(ThreadId, JsValue) + Send>
//!   __wwm_js_payload: any,                 // user-defined payload
//!   __wwm_sender_id: number                // sender's ThreadId
//! }
//! ```
//!
//! The callback fat pointer is heap-allocated by the sender and dropped by the receiver
//! after invocation.

use std::cell::{Cell, RefCell};

use js_sys::{Array, Object, Reflect};
use wasm_bindgen::prelude::*;
use web_sys::{
    DedicatedWorkerGlobalScope, MessageChannel, MessageEvent, MessagePort, Worker, WorkerOptions,
};

use crate::utils::{decompose_box, reconstruct_box};
use crate::web_mutex::{is_main_thread, is_worker_thread_cached};

/// Unified callback type for messages between any threads.
/// First argument is sender thread ID. Second argument is JS payload.
type ThreadCallback = dyn FnOnce(ThreadId, JsValue) + Send;

/// Assert that we're on the main thread, panic otherwise.
#[track_caller]
fn assert_main_thread() {
    if !is_main_thread() {
        let loc = std::panic::Location::caller();
        panic!(
            "{}:{} can only be called from the main thread",
            loc.file(),
            loc.line()
        );
    }
}

/// Assert that we're on a worker thread, panic otherwise.
#[track_caller]
fn assert_worker_thread() {
    if !is_worker_thread_cached() {
        let loc = std::panic::Location::caller();
        panic!(
            "{}:{} can only be called from a worker thread",
            loc.file(),
            loc.line()
        );
    }
}

/// Thread identifier. `ThreadId(0)` is main thread, `ThreadId(1..=n)` are workers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ThreadId(pub u32);

impl ThreadId {
    /// The main thread's ID.
    pub const MAIN: ThreadId = ThreadId(0);

    /// Returns true if this is the main thread.
    pub fn is_main(&self) -> bool {
        self.0 == 0
    }
}

/// Status of a web worker.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerStatus {
    /// Not yet spawned or invalid ThreadId.
    Uninitialized = 0,
    /// Spawned but haven't yet received finish init message.
    /// We can send task messages to workers in this status. Browser queues them.
    Initializing = 1,
    /// The finish init message has been received.
    Normal = 2,
    /// Currently doing dynamic linking (not implemented yet).
    DynamicLinking = 3,
}

impl WorkerStatus {
    fn from_u8(v: u8) -> Self {
        match v {
            1 => WorkerStatus::Initializing,
            2 => WorkerStatus::Normal,
            3 => WorkerStatus::DynamicLinking,
            _ => WorkerStatus::Uninitialized,
        }
    }
}

/// Cache-line padded atomic for worker status to avoid false sharing.
#[repr(C, align(64))]
struct PaddedAtomicU8 {
    value: std::sync::atomic::AtomicU8,
    _padding: [u8; 63],
}

impl PaddedAtomicU8 {
    fn new(v: u8) -> Self {
        Self {
            value: std::sync::atomic::AtomicU8::new(v),
            _padding: [0; 63],
        }
    }
}

/// Global worker status array. Late-initialized during `init_web_worker_manager`.
/// Pointer to leaked Box<[PaddedAtomicU8]> for 'static lifetime.
static WORKER_STATUS: std::sync::atomic::AtomicPtr<PaddedAtomicU8> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

/// Number of workers (set during init).
static THREAD_COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Initialize the global worker status array. Called once during init.
fn init_worker_status_array(count: usize) {
    let mut vec: Vec<PaddedAtomicU8> = Vec::with_capacity(count);
    for _ in 0..count {
        vec.push(PaddedAtomicU8::new(WorkerStatus::Uninitialized as u8));
    }
    let boxed: Box<[PaddedAtomicU8]> = vec.into_boxed_slice();
    let leaked: &'static mut [PaddedAtomicU8] = Box::leak(boxed);

    WORKER_STATUS.store(leaked.as_mut_ptr(), std::sync::atomic::Ordering::Release);
    THREAD_COUNT.store(count, std::sync::atomic::Ordering::Release);
}

/// Get the worker status array. Returns None if not initialized.
fn get_worker_status_array() -> Option<&'static [PaddedAtomicU8]> {
    let ptr = WORKER_STATUS.load(std::sync::atomic::Ordering::Acquire);
    if ptr.is_null() {
        return None;
    }
    let count = THREAD_COUNT.load(std::sync::atomic::Ordering::Acquire);
    // SAFETY: ptr was leaked from a Box<[PaddedAtomicU8]> with `count` elements
    Some(unsafe { std::slice::from_raw_parts(ptr, count) })
}

/// Get the status of a worker. Can be called from any thread.
pub fn get_worker_status(thread_id: ThreadId) -> WorkerStatus {
    if thread_id.is_main() {
        return WorkerStatus::Normal;
    }
    let Some(arr) = get_worker_status_array() else {
        return WorkerStatus::Uninitialized;
    };
    let idx = (thread_id.0 - 1) as usize; // ThreadId 1 -> index 0
    if idx >= arr.len() {
        return WorkerStatus::Uninitialized;
    }
    let v = arr[idx].value.load(std::sync::atomic::Ordering::Acquire);
    WorkerStatus::from_u8(v)
}

/// Set the status of a worker. Can be called from any thread.
/// No-op if worker manager not initialized or invalid ThreadId.
pub fn set_worker_status(thread_id: ThreadId, status: WorkerStatus) {
    if thread_id.is_main() {
        return;
    }
    let Some(arr) = get_worker_status_array() else {
        return;
    };
    let idx = (thread_id.0 - 1) as usize; // ThreadId 1 -> index 0
    if idx >= arr.len() {
        return;
    }
    arr[idx]
        .value
        .store(status as u8, std::sync::atomic::Ordering::Release);
}

/// Internal state for a single worker (main thread only).
struct WorkerStateForMainThread {
    worker: Worker,
    /// The onmessage closure, kept alive to prevent GC.
    _onmessage_closure: Closure<dyn FnMut(MessageEvent)>,
}

/// Main thread state for the web worker manager.
struct MainThreadState {
    /// Worker states indexed by ThreadId (1-indexed, so index 0 is unused).
    workers: Vec<Option<WorkerStateForMainThread>>,
    /// Closures for MessageChannel onmessage handlers (kept alive to prevent GC).
    /// Not used on main thread, but stored here during init before transfer.
    _channel_closures: Vec<Closure<dyn FnMut(MessageEvent)>>,
}

/// Worker thread state (thread-local on each worker).
struct WorkerThreadState {
    /// MessagePorts to other workers, indexed by ThreadId.
    /// ports[tid] = port to communicate with ThreadId(tid)
    /// - ports[0] = None (main thread uses postMessage, not MessageChannel)
    /// - ports[my_tid] = None (no port to self)
    ports: Vec<Option<MessagePort>>,
    /// Closures for MessageChannel onmessage handlers (kept alive to prevent GC).
    _port_closures: Vec<Closure<dyn FnMut(MessageEvent)>>,
}

thread_local! {
    /// Main thread state (only initialized on main thread).
    static MAIN_THREAD_STATE: RefCell<Option<MainThreadState>> = const { RefCell::new(None) };

    /// Worker thread state (only initialized on worker threads).
    static WORKER_THREAD_STATE: RefCell<Option<WorkerThreadState>> = const { RefCell::new(None) };

    /// Current thread's ThreadId. Set on all threads.
    static MY_THREAD_ID: Cell<Option<ThreadId>> = const { Cell::new(None) };
}

/// Get the current thread's ThreadId. Panics if not set.
#[track_caller]
pub fn my_thread_id() -> ThreadId {
    MY_THREAD_ID.with(|id| {
        id.get()
            .expect("ThreadId not set - web worker manager not initialized")
    })
}

/// Get the number of workers. Must be called after init.
#[track_caller]
pub fn worker_count() -> usize {
    if is_main_thread() {
        MAIN_THREAD_STATE.with(|state| {
            state
                .borrow()
                .as_ref()
                .expect("WebWorkerManager not initialized")
                .workers
                .len()
        })
    } else {
        WORKER_THREAD_STATE.with(|state| {
            state
                .borrow()
                .as_ref()
                .expect("Worker not initialized")
                .ports
                .len()
        })
    }
}

/// Access main thread state. Panics if not initialized or not on main thread.
fn with_main_state<F, R>(f: F) -> R
where
    F: FnOnce(&MainThreadState) -> R,
{
    MAIN_THREAD_STATE.with(|state| {
        let state = state.borrow();
        let state = state.as_ref().expect("WebWorkerManager not initialized");
        f(state)
    })
}

/// Access main thread state mutably. Panics if not initialized or not on main thread.
fn with_main_state_mut<F, R>(f: F) -> R
where
    F: FnOnce(&mut MainThreadState) -> R,
{
    MAIN_THREAD_STATE.with(|state| {
        let mut state = state.borrow_mut();
        let state = state.as_mut().expect("WebWorkerManager not initialized");
        f(state)
    })
}

/// Create the `__wwm_callback` JS array from a fat pointer.
fn create_callback_array(data_ptr: *mut (), vtable_ptr: *mut ()) -> Array {
    let arr = Array::new_with_length(2);
    arr.set(0, JsValue::from_f64(data_ptr as usize as f64));
    arr.set(1, JsValue::from_f64(vtable_ptr as usize as f64));
    arr
}

/// Parse the `__wwm_callback` JS array into fat pointer components.
fn parse_callback_array(arr: &JsValue) -> Option<(*mut (), *mut ())> {
    let arr: &Array = arr.dyn_ref()?;
    let data_ptr = arr.get(0).as_f64()? as usize as *mut ();
    let vtable_ptr = arr.get(1).as_f64()? as usize as *mut ();
    Some((data_ptr, vtable_ptr))
}

/// Initialize the web worker manager with a fixed number of workers.
/// Must be called from main thread. Panics if called twice.
///
/// This creates all workers upfront and sets up n² MessageChannels for
/// worker-to-worker communication.
///
/// The `on_init` callback runs in each worker after WASM init completes.
/// It receives the worker's ThreadId as JS payload.
#[track_caller]
pub fn init_web_worker_manager(
    worker_count: usize,
    on_init: Box<dyn Fn(ThreadId) + Send + Sync>,
) -> Result<(), JsValue> {
    assert_main_thread();

    MAIN_THREAD_STATE.with(|state| {
        if state.borrow().is_some() {
            panic!("WebWorkerManager already initialized");
        }
    });

    MY_THREAD_ID.with(|id| id.set(Some(ThreadId::MAIN)));

    let thread_count = worker_count + 1;

    init_worker_status_array(thread_count);

    // Create MessageChannels that allow each web worker to send message to any web worker.
    // ports[i][j] stores the port for worker i to communicate with j (i != j).
    // Index 0 row/col correspond to main thread. sending message to and from main thread doesn't use MessageChannel.
    let mut ports: Vec<Vec<Option<MessagePort>>> = vec![vec![None; thread_count]; thread_count];
    for i in 1..thread_count {
        for j in (i + 1)..thread_count {
            let channel = MessageChannel::new()?;
            ports[i][j] = Some(channel.port1());
            ports[j][i] = Some(channel.port2());
        }
    }

    // Create workers
    let mut workers: Vec<Option<WorkerStateForMainThread>> = Vec::with_capacity(thread_count);
    workers.push(None); // Index 0 unused (main thread)

    let on_init = std::sync::Arc::new(on_init);

    for worker_idx in 0..worker_count {
        let thread_id = ThreadId((worker_idx + 1) as u32);
        let my_tid = thread_id.0 as usize;

        let worker_options = WorkerOptions::new();
        worker_options.set_type(web_sys::WorkerType::Module);
        let worker = Worker::new_with_options("./worker.js", &worker_options)?;

        // Set up onmessage handler for this worker (for messages from worker to main)
        let thread_id_for_closure = thread_id;
        let onmessage_closure =
            Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
                handle_message_from_worker(thread_id_for_closure, event);
            });
        worker.set_onmessage(Some(onmessage_closure.as_ref().unchecked_ref()));

        let worker_ports = Array::new();

        for other_tid in 0..thread_count {
            if let Some(ref port) = ports[my_tid][other_tid] {
                worker_ports.push(port);
            } else {
                worker_ports.push(&JsValue::NULL);
            }
        }

        // Create init callback that runs on worker
        let on_init_clone = on_init.clone();
        let wrapped_on_init: Box<dyn FnOnce(ThreadId, JsValue) + Send> =
            Box::new(move |sender_id, _js_payload| {
                // sender_id is ThreadId::MAIN for init message
                let _ = sender_id;

                // Get my thread ID from thread-local (set by JS during init)
                let my_id = my_thread_id();

                // Run user's init callback
                on_init_clone(my_id);

                // Update status to Normal (global atomic, no message passing needed)
                set_worker_status(my_id, WorkerStatus::Normal);
            });

        // Decompose the callback fat pointer
        let (data_ptr, vtable_ptr) = decompose_box(wrapped_on_init);

        // Send init message
        let msg = Object::new();
        Reflect::set(&msg, &"__wwm_wasm_module".into(), &wasm_bindgen::module())?;
        Reflect::set(&msg, &"__wwm_wasm_memory".into(), &wasm_bindgen::memory())?;
        Reflect::set(
            &msg,
            &"__wwm_callback".into(),
            &create_callback_array(data_ptr, vtable_ptr),
        )?;
        Reflect::set(&msg, &"__wwm_js_payload".into(), &JsValue::UNDEFINED)?;
        Reflect::set(
            &msg,
            &"__wwm_thread_id".into(),
            &JsValue::from_f64(thread_id.0 as f64),
        )?;
        Reflect::set(
            &msg,
            &"__wwm_sender_id".into(),
            &JsValue::from_f64(ThreadId::MAIN.0 as f64),
        )?;
        // MessageChannel ports (indexed by thread_id)
        Reflect::set(&msg, &"__wwm_ports".into(), &worker_ports)?;
        Reflect::set(
            &msg,
            &"__wwm_thread_count".into(),
            &JsValue::from_f64(thread_count as f64),
        )?;

        let transfer = Array::new();
        for other_tid in 0..thread_count {
            if let Some(ref port) = ports[my_tid][other_tid] {
                transfer.push(port);
            }
        }

        worker.post_message_with_transfer(&msg, &transfer)?;

        // Set status to Initializing via global atomic
        set_worker_status(thread_id, WorkerStatus::Initializing);

        workers.push(Some(WorkerStateForMainThread {
            worker,
            _onmessage_closure: onmessage_closure,
        }));
    }

    // Store state
    MAIN_THREAD_STATE.with(|state| {
        *state.borrow_mut() = Some(MainThreadState {
            workers,
            _channel_closures: Vec::new(),
        });
    });

    Ok(())
}

/// Handle a message received from a worker on main thread.
fn handle_message_from_worker(sender_id: ThreadId, event: MessageEvent) {
    let data = event.data();

    // Check for __wwm_callback field (task message)
    let callback_arr = Reflect::get(&data, &"__wwm_callback".into()).ok();

    if let Some(ref arr) = callback_arr {
        if !arr.is_undefined() && !arr.is_null() {
            if let Some((data_ptr, vtable_ptr)) = parse_callback_array(arr) {
                let js_payload =
                    Reflect::get(&data, &"__wwm_js_payload".into()).unwrap_or(JsValue::UNDEFINED);

                // Reconstruct fat pointer and call the callback
                let callback: Box<ThreadCallback> =
                    unsafe { reconstruct_box(data_ptr, vtable_ptr) };
                callback(sender_id, js_payload);
                return;
            }
        }
    }

    // Unknown message format
    web_sys::console::error_1(
        &format!(
            "[WWM] Unknown message format from worker {:?}: {:?}",
            sender_id, data
        )
        .into(),
    );
}

/// Handle a message received via MessageChannel (worker-to-worker).
fn handle_message_from_channel(sender_id: ThreadId, event: MessageEvent) {
    let data = event.data();

    let callback_arr = Reflect::get(&data, &"__wwm_callback".into()).ok();

    if let Some(ref arr) = callback_arr {
        if !arr.is_undefined() && !arr.is_null() {
            if let Some((data_ptr, vtable_ptr)) = parse_callback_array(arr) {
                let js_payload =
                    Reflect::get(&data, &"__wwm_js_payload".into()).unwrap_or(JsValue::UNDEFINED);

                let callback: Box<ThreadCallback> =
                    unsafe { reconstruct_box(data_ptr, vtable_ptr) };
                callback(sender_id, js_payload);
                return;
            }
        }
    }

    web_sys::console::error_1(
        &format!(
            "[WWM] Unknown message format from channel (sender {:?}): {:?}",
            sender_id, data
        )
        .into(),
    );
}

/// Send a message to any thread (main or worker).
///
/// The `callback` will be invoked on the target thread with sender's ThreadId and js_payload.
///
/// The optional `transfer` array specifies objects to transfer (not clone) to the target thread.
/// Use this for transferable objects like `OffscreenCanvas`, `MessagePort`, `ArrayBuffer`, etc.
///
/// Routing:
/// - Main to main: uses `queueMicrotask`
/// - Main to worker: uses `Worker.postMessage()`
/// - Worker to main: uses `self.postMessage()`
/// - Worker to worker: uses MessageChannel
#[track_caller]
pub fn send_to_thread(
    target: ThreadId,
    callback: Box<ThreadCallback>,
    js_payload: &JsValue,
    transfer: Option<&Array>,
) -> Result<(), JsValue> {
    let my_id = my_thread_id();

    if my_id == target {
        // Thread sending to itself. Use queueMicrotask
        let js_payload_clone = js_payload.clone();
        let closure = Closure::once_into_js(move || {
            callback(my_id, js_payload_clone);
        });

        if my_id.is_main() {
            let window = web_sys::window().expect("no window");
            window.queue_microtask(closure.unchecked_ref());
        } else {
            let global = js_sys::global().unchecked_into::<DedicatedWorkerGlobalScope>();
            global.queue_microtask(closure.unchecked_ref());
        }
        return Ok(());
    }

    // Create JS message
    let (data_ptr, vtable_ptr) = decompose_box(callback);
    let msg = Object::new();
    Reflect::set(
        &msg,
        &"__wwm_callback".into(),
        &create_callback_array(data_ptr, vtable_ptr),
    )?;
    Reflect::set(&msg, &"__wwm_js_payload".into(), js_payload)?;
    Reflect::set(
        &msg,
        &"__wwm_sender_id".into(),
        &JsValue::from_f64(my_id.0 as f64),
    )?;

    if my_id.is_main() {
        // Main thread sending to worker
        with_main_state(|state| {
            let worker_state = state
                .workers
                .get(target.0 as usize)
                .and_then(|s| s.as_ref())
                .ok_or_else(|| JsValue::from_str("Worker not found"))?;

            if let Some(transfer) = transfer {
                worker_state
                    .worker
                    .post_message_with_transfer(&msg, transfer)?;
            } else {
                worker_state.worker.post_message(&msg)?;
            }
            Ok(())
        })
    } else if target.is_main() {
        // Worker sending to main thread
        let global = js_sys::global().unchecked_into::<DedicatedWorkerGlobalScope>();
        if let Some(transfer) = transfer {
            global.post_message_with_transfer(&msg, transfer)?;
        } else {
            global.post_message(&msg)?;
        }
        Ok(())
    } else {
        // Worker sending to another worker via MessageChannel
        WORKER_THREAD_STATE.with(|state| {
            let state = state.borrow();
            let state = state.as_ref().expect("Worker not initialized");

            // ports[tid] = bidirectional port to ThreadId(tid)
            let port = state
                .ports
                .get(target.0 as usize)
                .and_then(|p| p.as_ref())
                .ok_or_else(|| JsValue::from_str("No MessagePort to target worker"))?;

            if let Some(transfer) = transfer {
                port.post_message_with_transferable(&msg, transfer)?;
            } else {
                port.post_message(&msg)?;
            }
            Ok(())
        })
    }
}

/// Initialize worker thread state. Called from worker.js after WASM init.
/// Internal function - do not call directly.
#[wasm_bindgen(js_name = __wwm_internal_worker_init)]
pub fn __wwm_internal_worker_init(thread_id: u32, ports_js: JsValue, thread_count: u32) {
    assert_worker_thread();

    let thread_id = ThreadId(thread_id);

    // Set thread ID
    MY_THREAD_ID.with(|id| id.set(Some(thread_id)));

    // ports[tid] = port to communicate with ThreadId(tid)
    // - ports[0] = null (main thread uses postMessage, not MessageChannel)
    // - ports[my_tid] = null (no port to self)
    // Each port is bidirectional (same port for send and receive).
    let ports_arr: Array = ports_js.unchecked_into();
    let mut ports: Vec<Option<MessagePort>> = Vec::with_capacity(thread_count as usize);
    let mut port_closures: Vec<Closure<dyn FnMut(MessageEvent)>> = Vec::new();

    for tid in 0..thread_count {
        let port_val = ports_arr.get(tid);
        if port_val.is_null() || port_val.is_undefined() {
            ports.push(None);
        } else {
            let port: MessagePort = port_val.unchecked_into();

            // Set up onmessage handler for this port
            // Messages from ThreadId(tid) arrive on this port
            let sender_id = ThreadId(tid);
            let closure = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
                handle_message_from_channel(sender_id, event);
            });

            port.set_onmessage(Some(closure.as_ref().unchecked_ref()));
            port.start(); // Start receiving messages
            port_closures.push(closure);

            ports.push(Some(port));
        }
    }

    // Store state
    WORKER_THREAD_STATE.with(|state| {
        *state.borrow_mut() = Some(WorkerThreadState {
            ports,
            _port_closures: port_closures,
        });
    });
}

/// Handler for task messages received by a worker. Called from worker.js.
/// Internal function - do not call directly.
///
/// `sender_id` is the sender's ThreadId.
/// `callback_arr` is the `__wwm_callback` JS array containing [data_ptr, vtable_ptr].
/// `js_payload` is the `__wwm_js_payload` value.
#[wasm_bindgen(js_name = __wwm_internal_worker_handle_message)]
pub fn __wwm_internal_worker_handle_message(
    sender_id: u32,
    callback_arr: JsValue,
    js_payload: JsValue,
) {
    assert_worker_thread();

    let sender_id = ThreadId(sender_id);

    if let Some((data_ptr, vtable_ptr)) = parse_callback_array(&callback_arr) {
        let callback: Box<ThreadCallback> = unsafe { reconstruct_box(data_ptr, vtable_ptr) };
        callback(sender_id, js_payload);
    } else {
        web_sys::console::error_1(&"[WWM] Invalid callback array format".into());
    }
}
