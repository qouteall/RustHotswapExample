//! Web Worker Manager for managing web workers and message passing.
//!
//! This module provides a centralized manager for web workers that:
//! - Manages spawning of web workers
//! - Handles message passing between main thread and workers
//! - Tracks worker status
//! - Supports custom messages with JS payloads and Rust callbacks
//! - Support dynamic linking and hotswap (not yet implemented)
//!
//! Note: the web workers managed by it should not be touched by raw JS API.
//! Don't directly send JS message or change their JS callback.
//!
//! ## Message Protocol
//!
//! From main thread to web worker:
//! - Init message: `{ __wwm_wasm_module, __wwm_wasm_memory, __wwm_callback, __wwm_js_payload, __wwm_web_worker_id }`
//! - Task message: `{ __wwm_callback, __wwm_js_payload }`
//!
//! From web worker to main thread:
//! - Task message: `{ __wwm_callback, __wwm_js_payload }`
//!
//! `__wwm_callback` is a JS array of [dataPtr, vTablePtr], representing `Box<dyn FnOnce(JsValue) + Send>` fat pointer.

use std::cell::RefCell;
use std::collections::HashMap;

use js_sys::{Array, Object, Reflect};
use wasm_bindgen::prelude::*;
use web_sys::{DedicatedWorkerGlobalScope, MessageEvent, Worker, WorkerOptions};

use crate::utils::{decompose_box, reconstruct_box};
use crate::web_mutex::{is_main_thread_cached, is_worker_thread_cached};

// Type aliases for callback trait objects
// Using FnOnce + Send as callbacks cross thread boundaries and are called once
type WorkerCallback = dyn FnOnce(JsValue) + Send;
type MainCallback = dyn FnOnce(WorkerId, JsValue) + Send;

/// Assert that we're on the main thread, panic otherwise.
#[track_caller]
fn assert_main_thread() {
    if !is_main_thread_cached() {
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

/// Wrapper type for web worker IDs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WorkerId(pub u32);

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

/// Internal state for a single worker.
struct WorkerState {
    worker: Worker,
    status: WorkerStatus,
    /// The onmessage closure, kept alive to prevent GC.
    _onmessage_closure: Closure<dyn FnMut(MessageEvent)>,
}

/// The web worker manager, managed by main thread.
struct WebWorkerManager {
    workers: HashMap<WorkerId, WorkerState>,
    next_worker_id: u32,
}

impl WebWorkerManager {
    fn new() -> Self {
        Self {
            workers: HashMap::new(),
            next_worker_id: 0,
        }
    }

    fn allocate_worker_id(&mut self) -> WorkerId {
        let id = WorkerId(self.next_worker_id);
        self.next_worker_id += 1;
        id
    }

    /// Update worker status. Called internally.
    pub fn set_worker_status(&mut self, worker_id: WorkerId, status: WorkerStatus) {
        if let Some(state) = self.workers.get_mut(&worker_id) {
            state.status = status;
        }
    }
}

thread_local! {
    static WEB_WORKER_MANAGER: RefCell<Option<WebWorkerManager>> = const { RefCell::new(None) };
}

/// Initialize the web worker manager. Must be called from main thread.
/// Safe to call multiple times (will be no-op if already initialized).
#[track_caller]
pub fn init_web_worker_manager() {
    assert_main_thread();
    WEB_WORKER_MANAGER.with(|manager| {
        let mut manager = manager.borrow_mut();
        if manager.is_none() {
            *manager = Some(WebWorkerManager::new());
        }
    });
}

/// Access the web worker manager. Panics if not initialized or not on main thread.
fn with_manager<F, R>(f: F) -> R
where
    F: FnOnce(&WebWorkerManager) -> R,
{
    WEB_WORKER_MANAGER.with(|manager| {
        let manager = manager.borrow();
        let manager = manager.as_ref().expect("WebWorkerManager not initialized");
        f(manager)
    })
}

/// Access the web worker manager mutably. Panics if not initialized or not on main thread.
fn with_manager_mut<F, R>(f: F) -> R
where
    F: FnOnce(&mut WebWorkerManager) -> R,
{
    WEB_WORKER_MANAGER.with(|manager| {
        let mut manager = manager.borrow_mut();
        let manager = manager.as_mut().expect("WebWorkerManager not initialized");
        f(manager)
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

/// Spawn a new web worker. Returns the worker ID immediately (status will be Initializing).
/// Must be called from main thread.
///
/// The `on_init` callback runs in the worker immediately after WASM init completes.
/// The callback sends a message to main thread to update status to Normal.
///
/// Note: the web workers managed by it must be used via web worker manager API.
/// Don't use raw JS API for sending message or setting callback.
#[track_caller]
pub fn spawn_worker(
    on_init: Box<dyn FnOnce(JsValue) + Send>,
    js_payload: &JsValue,
) -> Result<WorkerId, JsValue> {
    assert_main_thread();
    with_manager_mut(|manager| {
        let worker_id = manager.allocate_worker_id();

        // Create worker options for ES module worker
        let options = WorkerOptions::new();
        options.set_type(web_sys::WorkerType::Module);

        let worker = Worker::new_with_options("./worker.js", &options)?;

        // Set up onmessage handler for this worker
        // The closure captures worker_id so main thread knows which worker sent the message
        let worker_id_for_closure = worker_id;
        let onmessage_closure = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
            handle_worker_message(worker_id_for_closure, event);
        });
        worker.set_onmessage(Some(onmessage_closure.as_ref().unchecked_ref()));

        // Decompose the init callback fat pointer
        let (data_ptr, vtable_ptr) = decompose_box(on_init);

        // Send init message with new protocol
        let msg = Object::new();
        Reflect::set(&msg, &"__wwm_wasm_module".into(), &wasm_bindgen::module())?;
        Reflect::set(&msg, &"__wwm_wasm_memory".into(), &wasm_bindgen::memory())?;
        Reflect::set(&msg, &"__wwm_callback".into(), &create_callback_array(data_ptr, vtable_ptr))?;
        Reflect::set(&msg, &"__wwm_js_payload".into(), js_payload)?;
        Reflect::set(&msg, &"__wwm_web_worker_id".into(), &JsValue::from_f64(worker_id.0 as f64))?;
        worker.post_message(&msg)?;

        manager.workers.insert(
            worker_id,
            WorkerState {
                worker,
                status: WorkerStatus::Initializing,
                _onmessage_closure: onmessage_closure,
            },
        );

        Ok(worker_id)
    })
}

/// Handle a message received from a worker (called on main thread).
fn handle_worker_message(worker_id: WorkerId, event: MessageEvent) {
    let data = event.data();

    // Check for __wwm_callback field (task message)
    let callback_arr = Reflect::get(&data, &"__wwm_callback".into()).ok();

    if let Some(ref arr) = callback_arr {
        if !arr.is_undefined() && !arr.is_null() {
            // This is a task message
            if let Some((data_ptr, vtable_ptr)) = parse_callback_array(arr) {
                let js_payload = Reflect::get(&data, &"__wwm_js_payload".into())
                    .unwrap_or(JsValue::UNDEFINED);

                // Reconstruct fat pointer and call the callback
                let callback: Box<MainCallback> = unsafe { reconstruct_box(data_ptr, vtable_ptr) };
                callback(worker_id, js_payload);
                // callback is dropped here
                return;
            }
        }
    }

    // Unknown message format
    web_sys::console::error_1(
        &format!("[WWM] Unknown message format from worker {:?}: {:?}", worker_id, data).into(),
    );
}

/// Send a task message from main thread to a worker.
///
/// The `callback` will be invoked on the worker side with the `js_payload`.
/// Must be called from main thread.
#[track_caller]
pub fn send_to_worker(
    worker_id: WorkerId,
    callback: Box<WorkerCallback>,
    js_payload: &JsValue,
) -> Result<(), JsValue> {
    assert_main_thread();
    with_manager(|manager| {
        let state = manager
            .workers
            .get(&worker_id)
            .ok_or_else(|| JsValue::from_str("Worker not found"))?;

        // Decompose fat pointer
        let (data_ptr, vtable_ptr) = decompose_box(callback);

        // Create task message
        let msg = Object::new();
        Reflect::set(&msg, &"__wwm_callback".into(), &create_callback_array(data_ptr, vtable_ptr))?;
        Reflect::set(&msg, &"__wwm_js_payload".into(), js_payload)?;

        state.worker.post_message(&msg)?;

        Ok(())
    })
}

/// Send a task message from worker to main thread.
///
/// The `callback` will be invoked on the main thread with worker ID and js_payload.
/// Must be called from a worker thread.
#[track_caller]
pub fn send_to_main(
    callback: Box<MainCallback>,
    js_payload: &JsValue,
) -> Result<(), JsValue> {
    assert_worker_thread();
    let global = js_sys::global().unchecked_into::<DedicatedWorkerGlobalScope>();

    // Decompose fat pointer
    let (data_ptr, vtable_ptr) = decompose_box(callback);

    // Create task message
    let msg = Object::new();
    Reflect::set(&msg, &"__wwm_callback".into(), &create_callback_array(data_ptr, vtable_ptr))?;
    Reflect::set(&msg, &"__wwm_js_payload".into(), js_payload)?;

    global.post_message(&msg)?;

    Ok(())
}

/// Get the status of a worker. Must be called from main thread.
#[track_caller]
pub fn get_worker_status(worker_id: WorkerId) -> Option<WorkerStatus> {
    assert_main_thread();
    with_manager(|manager| manager.workers.get(&worker_id).map(|s| s.status))
}

/// Get all worker IDs. Must be called from main thread.
#[track_caller]
pub fn get_all_worker_ids() -> Vec<WorkerId> {
    assert_main_thread();
    with_manager(|manager| manager.workers.keys().copied().collect())
}

/// Handler for task messages received by a worker. Called from worker.js.
/// Internal function. Do not call directly. (It's public because wasm-bindgen can only export public functions to JS)
///
/// `callback_arr` is the `__wwm_callback` JS array containing [data_ptr, vtable_ptr].
/// `js_payload` is the `__wwm_js_payload` value.
///
/// Must be called from a worker thread.
#[wasm_bindgen(js_name = __wwm_internal_worker_handle_message)]
pub fn __wwm_internal_worker_handle_message(callback_arr: JsValue, js_payload: JsValue) {
    assert_worker_thread();

    if let Some((data_ptr, vtable_ptr)) = parse_callback_array(&callback_arr) {
        // Reconstruct fat pointer and call the callback
        let callback: Box<WorkerCallback> = unsafe { reconstruct_box(data_ptr, vtable_ptr) };
        callback(js_payload);
        // callback is dropped here
    } else {
        web_sys::console::error_1(&"[WWM] Invalid callback array format".into());
    }
}
