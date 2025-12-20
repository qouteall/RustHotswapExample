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
//! - Also wasm-bindgen JS proxy types (e.g., `JsValue`)
//!   only hold an index into a JS-side array. The actual JS value cannot cross
//!   thread boundaries via shared memory - it must be sent via `postMessage`.
//!   Related: <https://wasm-bindgen.github.io/wasm-bindgen/contributing/design/js-objects-in-rust.html>
//!
//! ## Why Web Worker Manager?
//!
//! This module provides a centralized manager to work around these limitations:
//!
//! - Manage worker references and status from main thread,
//!   since there's no built-in way to enumerate workers.
//! - Handle WASM module/memory transfer and worker
//!   initialization sequence.
//! - Provide a consistent way to send Rust closures
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
//!   __wwm_outbound_ports: [MessagePort | null, ...],  // ports to send to other workers
//!   __wwm_inbound_ports: [MessagePort | null, ...],   // ports to receive from other workers
//!   __wwm_worker_count: number             // total number of workers
//! }
//! ```
//!
//! Port arrays:
//! `outbound_ports[i]` is the port to send to worker `i+1`.
//! `inbound_ports[i]` is the port to receive from worker `i+1`.
//! Self-referencing entries are `null` (e.g., worker 1's `outbound_ports[0]` is null).
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
use web_sys::{DedicatedWorkerGlobalScope, MessageChannel, MessageEvent, MessagePort, Worker, WorkerOptions};

use crate::utils::{decompose_box, reconstruct_box};
use crate::web_mutex::{is_main_thread, is_worker_thread_cached};

/// Unified callback type for messages between any threads.
/// Receives sender's ThreadId and the JS payload.
type ThreadCallback = dyn FnOnce(ThreadId, JsValue) + Send;

/// Assert that we're on the main thread, panic otherwise.
#[track_caller]
fn assert_main_thread() {
    if !is_main_thread() {
        let loc = std::panic::Location::caller();
        panic!("{}:{} can only be called from the main thread", loc.file(), loc.line());
    }
}

/// Assert that we're on a worker thread, panic otherwise.
#[track_caller]
fn assert_worker_thread() {
    if !is_worker_thread_cached() {
        let loc = std::panic::Location::caller();
        panic!("{}:{} can only be called from a worker thread", loc.file(), loc.line());
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerStatus {
    /// Spawned but haven't yet received finish init message.
    /// We can send task messages to workers in this status. Browser queues them.
    Initializing,
    /// The finish init message has been received.
    Normal,
    /// Currently doing dynamic linking (not implemented yet).
    DynamicLinking,
    /// Gracefully exiting (not implemented yet).
    Finalizing,
}

/// Internal state for a single worker (main thread only).
struct WorkerState {
    worker: Worker,
    status: WorkerStatus,
    /// The onmessage closure, kept alive to prevent GC.
    _onmessage_closure: Closure<dyn FnMut(MessageEvent)>,
}

/// Main thread state for the web worker manager.
struct MainThreadState {
    /// Worker states indexed by ThreadId (1-indexed, so index 0 is unused).
    workers: Vec<Option<WorkerState>>,
    /// Number of workers.
    worker_count: usize,
    /// Closures for MessageChannel onmessage handlers (kept alive to prevent GC).
    /// Not used on main thread, but stored here during init before transfer.
    _channel_closures: Vec<Closure<dyn FnMut(MessageEvent)>>,
}

/// Worker thread state (thread-local on each worker).
struct WorkerThreadState {
    /// This worker's ThreadId.
    my_thread_id: ThreadId,
    /// Outbound MessagePorts to other workers. Index i = port to worker i+1.
    /// Index 0 is unused (no port to main thread via MessageChannel).
    outbound_ports: Vec<Option<MessagePort>>,
    /// Closures for inbound MessageChannel onmessage handlers (kept alive to prevent GC).
    _inbound_closures: Vec<Closure<dyn FnMut(MessageEvent)>>,
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
    MY_THREAD_ID.with(|id| id.get().expect("ThreadId not set - web worker manager not initialized"))
}

/// Get the number of workers. Must be called after init.
#[track_caller]
pub fn worker_count() -> usize {
    if is_main_thread() {
        MAIN_THREAD_STATE.with(|state| {
            state.borrow().as_ref().expect("WebWorkerManager not initialized").worker_count
        })
    } else {
        WORKER_THREAD_STATE.with(|state| {
            state.borrow().as_ref().expect("Worker not initialized").outbound_ports.len()
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

    // Check not already initialized
    MAIN_THREAD_STATE.with(|state| {
        if state.borrow().is_some() {
            panic!("WebWorkerManager already initialized");
        }
    });

    // Set main thread's ID
    MY_THREAD_ID.with(|id| id.set(Some(ThreadId::MAIN)));

    // Create MessageChannels for worker-to-worker communication.
    // channels[i][j] = channel for worker i+1 to send to worker j+1 (0-indexed arrays)
    // We only need channels where i != j
    let mut channels: Vec<Vec<Option<MessageChannel>>> = Vec::with_capacity(worker_count);
    for i in 0..worker_count {
        let mut row = Vec::with_capacity(worker_count);
        for j in 0..worker_count {
            if i != j {
                row.push(Some(MessageChannel::new()?));
            } else {
                row.push(None); // No channel to self
            }
        }
        channels.push(row);
    }

    // Create workers
    let mut workers: Vec<Option<WorkerState>> = Vec::with_capacity(worker_count + 1);
    workers.push(None); // Index 0 unused (main thread)

    let on_init = std::sync::Arc::new(on_init);

    for worker_idx in 0..worker_count {
        let thread_id = ThreadId((worker_idx + 1) as u32);

        // Create worker options for ES module worker
        let options = WorkerOptions::new();
        options.set_type(web_sys::WorkerType::Module);

        let worker = Worker::new_with_options("./worker.js", &options)?;

        // Set up onmessage handler for this worker (for messages from worker to main)
        let thread_id_for_closure = thread_id;
        let onmessage_closure = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
            handle_message_from_worker(thread_id_for_closure, event);
        });
        worker.set_onmessage(Some(onmessage_closure.as_ref().unchecked_ref()));

        // Collect MessagePorts for this worker:
        // - outbound_ports: ports this worker uses to SEND to other workers
        //   outbound_ports[j] = port1 of channels[worker_idx][j] (for j != worker_idx)
        // - inbound_ports: ports this worker uses to RECEIVE from other workers
        //   inbound_ports[i] = port2 of channels[i][worker_idx] (for i != worker_idx)
        let outbound_ports = Array::new();
        let inbound_ports = Array::new();

        for j in 0..worker_count {
            if j != worker_idx {
                if let Some(ref channel) = channels[worker_idx][j] {
                    outbound_ports.push(&channel.port1());
                } else {
                    outbound_ports.push(&JsValue::NULL);
                }
            } else {
                outbound_ports.push(&JsValue::NULL); // No port to self
            }
        }

        for i in 0..worker_count {
            if i != worker_idx {
                if let Some(ref channel) = channels[i][worker_idx] {
                    inbound_ports.push(&channel.port2());
                } else {
                    inbound_ports.push(&JsValue::NULL);
                }
            } else {
                inbound_ports.push(&JsValue::NULL); // No port from self
            }
        }

        // Create init callback that runs on worker
        let on_init_clone = on_init.clone();
        let wrapped_on_init: Box<dyn FnOnce(ThreadId, JsValue) + Send> = Box::new(move |sender_id, _js_payload| {
            // sender_id is ThreadId::MAIN for init message
            let _ = sender_id;

            // Get my thread ID from thread-local (set by JS during init)
            let my_id = my_thread_id();

            // Run user's init callback
            on_init_clone(my_id);

            // Send status update to main thread
            // TODO review the purpose of state tracking. also can it be done without message passing
            let _ = send_to_thread(
                ThreadId::MAIN,
                Box::new(move |_sender, _payload| {
                    with_main_state_mut(|state| {
                        if let Some(Some(worker_state)) = state.workers.get_mut(my_id.0 as usize) {
                            worker_state.status = WorkerStatus::Normal;
                        }
                    });
                }),
                &JsValue::UNDEFINED,
            );
        });

        // Decompose the callback fat pointer
        let (data_ptr, vtable_ptr) = decompose_box(wrapped_on_init);

        // Send init message
        let msg = Object::new();
        Reflect::set(&msg, &"__wwm_wasm_module".into(), &wasm_bindgen::module())?;
        Reflect::set(&msg, &"__wwm_wasm_memory".into(), &wasm_bindgen::memory())?;
        Reflect::set(&msg, &"__wwm_callback".into(), &create_callback_array(data_ptr, vtable_ptr))?;
        Reflect::set(&msg, &"__wwm_js_payload".into(), &JsValue::UNDEFINED)?;
        Reflect::set(&msg, &"__wwm_thread_id".into(), &JsValue::from_f64(thread_id.0 as f64))?;
        Reflect::set(&msg, &"__wwm_sender_id".into(), &JsValue::from_f64(ThreadId::MAIN.0 as f64))?;
        // MessageChannel ports (separate from user payload)
        Reflect::set(&msg, &"__wwm_outbound_ports".into(), &outbound_ports)?;
        Reflect::set(&msg, &"__wwm_inbound_ports".into(), &inbound_ports)?;
        Reflect::set(&msg, &"__wwm_worker_count".into(), &JsValue::from_f64(worker_count as f64))?;

        // Transfer the MessagePorts
        let transfer = Array::new();
        for j in 0..worker_count {
            if j != worker_idx {
                if let Some(ref channel) = channels[worker_idx][j] {
                    transfer.push(&channel.port1());
                }
            }
        }
        for i in 0..worker_count {
            if i != worker_idx {
                if let Some(ref channel) = channels[i][worker_idx] {
                    transfer.push(&channel.port2());
                }
            }
        }

        worker.post_message_with_transfer(&msg, &transfer)?;

        workers.push(Some(WorkerState {
            worker,
            status: WorkerStatus::Initializing,
            _onmessage_closure: onmessage_closure,
        }));
    }

    // Store state
    MAIN_THREAD_STATE.with(|state| {
        *state.borrow_mut() = Some(MainThreadState {
            workers,
            worker_count,
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
                let js_payload = Reflect::get(&data, &"__wwm_js_payload".into())
                    .unwrap_or(JsValue::UNDEFINED);

                // Reconstruct fat pointer and call the callback
                let callback: Box<ThreadCallback> = unsafe { reconstruct_box(data_ptr, vtable_ptr) };
                callback(sender_id, js_payload);
                return;
            }
        }
    }

    // Unknown message format
    web_sys::console::error_1(
        &format!("[WWM] Unknown message format from worker {:?}: {:?}", sender_id, data).into(),
    );
}

/// Handle a message received via MessageChannel (worker-to-worker).
fn handle_message_from_channel(sender_id: ThreadId, event: MessageEvent) {
    let data = event.data();

    let callback_arr = Reflect::get(&data, &"__wwm_callback".into()).ok();

    if let Some(ref arr) = callback_arr {
        if !arr.is_undefined() && !arr.is_null() {
            if let Some((data_ptr, vtable_ptr)) = parse_callback_array(arr) {
                let js_payload = Reflect::get(&data, &"__wwm_js_payload".into())
                    .unwrap_or(JsValue::UNDEFINED);

                let callback: Box<ThreadCallback> = unsafe { reconstruct_box(data_ptr, vtable_ptr) };
                callback(sender_id, js_payload);
                return;
            }
        }
    }

    web_sys::console::error_1(
        &format!("[WWM] Unknown message format from channel (sender {:?}): {:?}", sender_id, data).into(),
    );
}

/// Send a message to any thread (main or worker).
///
/// The `callback` will be invoked on the target thread with sender's ThreadId and js_payload.
///
/// Routing:
/// - To main thread (ThreadId(0)): uses `self.postMessage()` from worker
/// - From main thread to worker: uses `Worker.postMessage()`
/// - From worker to worker: uses MessageChannel
#[track_caller]
pub fn send_to_thread(
    target: ThreadId,
    callback: Box<ThreadCallback>,
    js_payload: &JsValue,
) -> Result<(), JsValue> {
    let my_id = my_thread_id();

    // Decompose fat pointer
    let (data_ptr, vtable_ptr) = decompose_box(callback);

    // Create message
    let msg = Object::new();
    Reflect::set(&msg, &"__wwm_callback".into(), &create_callback_array(data_ptr, vtable_ptr))?;
    Reflect::set(&msg, &"__wwm_js_payload".into(), js_payload)?;
    Reflect::set(&msg, &"__wwm_sender_id".into(), &JsValue::from_f64(my_id.0 as f64))?;

    if my_id.is_main() {
        // Main thread sending to worker
        if target.is_main() {
            return Err(JsValue::from_str("Cannot send from main thread to itself"));
        }

        with_main_state(|state| {
            let worker_state = state.workers
                .get(target.0 as usize)
                .and_then(|s| s.as_ref())
                .ok_or_else(|| JsValue::from_str("Worker not found"))?;

            worker_state.worker.post_message(&msg)?;
            Ok(())
        })
    } else if target.is_main() {
        // Worker sending to main thread
        let global = js_sys::global().unchecked_into::<DedicatedWorkerGlobalScope>();
        global.post_message(&msg)?;
        Ok(())
    } else {
        // Worker sending to another worker via MessageChannel
        WORKER_THREAD_STATE.with(|state| {
            let state = state.borrow();
            let state = state.as_ref().expect("Worker not initialized");

            // outbound_ports index: target ThreadId - 1 (since workers are 1-indexed)
            let port_idx = (target.0 - 1) as usize;
            let port = state.outbound_ports
                .get(port_idx)
                .and_then(|p| p.as_ref())
                .ok_or_else(|| JsValue::from_str("No MessagePort to target worker"))?;

            port.post_message(&msg)?;
            Ok(())
        })
    }
}

/// Get the status of a worker. Must be called from main thread.
#[track_caller]
pub fn get_worker_status(thread_id: ThreadId) -> Option<WorkerStatus> {
    assert_main_thread();
    if thread_id.is_main() {
        return None; // Main thread doesn't have a WorkerStatus
    }
    with_main_state(|state| {
        state.workers
            .get(thread_id.0 as usize)
            .and_then(|s| s.as_ref())
            .map(|s| s.status)
    })
}

/// Get all worker ThreadIds. Must be called from main thread.
#[track_caller]
pub fn get_all_worker_ids() -> Vec<ThreadId> {
    assert_main_thread();
    with_main_state(|state| {
        (1..=state.worker_count)
            .map(|i| ThreadId(i as u32))
            .collect()
    })
}

/// Initialize worker thread state. Called from worker.js after WASM init.
/// Internal function - do not call directly.
#[wasm_bindgen(js_name = __wwm_internal_worker_init)]
pub fn __wwm_internal_worker_init(
    thread_id: u32,
    outbound_ports: JsValue,
    inbound_ports: JsValue,
    worker_count: u32,
) {
    assert_worker_thread();

    let thread_id = ThreadId(thread_id);

    // Set thread ID
    MY_THREAD_ID.with(|id| id.set(Some(thread_id)));

    // Parse outbound ports
    let outbound_arr: Array = outbound_ports.unchecked_into();
    let mut outbound: Vec<Option<MessagePort>> = Vec::with_capacity(worker_count as usize);
    for i in 0..worker_count {
        let port = outbound_arr.get(i);
        if port.is_null() || port.is_undefined() {
            outbound.push(None);
        } else {
            outbound.push(Some(port.unchecked_into()));
        }
    }

    // Parse inbound ports and set up onmessage handlers
    let inbound_arr: Array = inbound_ports.unchecked_into();
    let mut inbound_closures: Vec<Closure<dyn FnMut(MessageEvent)>> = Vec::new();

    for i in 0..worker_count {
        let port_val = inbound_arr.get(i);
        if !port_val.is_null() && !port_val.is_undefined() {
            let port: MessagePort = port_val.unchecked_into();
            let sender_id = ThreadId(i + 1); // Worker i+1 sends via this port

            let closure = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
                handle_message_from_channel(sender_id, event);
            });

            port.set_onmessage(Some(closure.as_ref().unchecked_ref()));
            port.start(); // Start receiving messages
            inbound_closures.push(closure);
        }
    }

    // Store state
    WORKER_THREAD_STATE.with(|state| {
        *state.borrow_mut() = Some(WorkerThreadState {
            my_thread_id: thread_id,
            outbound_ports: outbound,
            _inbound_closures: inbound_closures,
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
pub fn __wwm_internal_worker_handle_message(sender_id: u32, callback_arr: JsValue, js_payload: JsValue) {
    assert_worker_thread();

    let sender_id = ThreadId(sender_id);

    if let Some((data_ptr, vtable_ptr)) = parse_callback_array(&callback_arr) {
        let callback: Box<ThreadCallback> = unsafe { reconstruct_box(data_ptr, vtable_ptr) };
        callback(sender_id, js_payload);
    } else {
        web_sys::console::error_1(&"[WWM] Invalid callback array format".into());
    }
}
