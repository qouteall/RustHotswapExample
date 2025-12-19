//! Web Worker Manager for managing web workers and message passing.
//!
//! This module provides a centralized manager for web workers that:
//! - Manages spawning of web workers
//! - Handles message passing between main thread and workers
//! - Tracks worker status
//! - Supports custom messages with JS payloads and Rust callbacks

use std::cell::RefCell;
use std::collections::HashMap;
use std::mem;

use js_sys::{Object, Reflect};
use wasm_bindgen::prelude::*;
use web_sys::{DedicatedWorkerGlobalScope, MessageEvent, Window, Worker, WorkerOptions};

// Type aliases for callback trait objects
type WorkerCallback = dyn Fn(&JsValue);
type MainCallback = dyn Fn(&WebWorkerManager, WorkerId, &JsValue);

/// Check if running on main thread (Window context).
fn is_main_thread() -> bool {
    js_sys::global().dyn_into::<Window>().is_ok()
}

/// Check if running on a worker thread (DedicatedWorkerGlobalScope context).
fn is_worker_thread() -> bool {
    js_sys::global().dyn_into::<DedicatedWorkerGlobalScope>().is_ok()
}

/// Assert that we're on the main thread, panic otherwise.
fn assert_main_thread(fn_name: &str) {
    if !is_main_thread() {
        panic!("{} can only be called from the main thread", fn_name);
    }
}

/// Assert that we're on a worker thread, panic otherwise.
fn assert_worker_thread(fn_name: &str) {
    if !is_worker_thread() {
        panic!("{} can only be called from a worker thread", fn_name);
    }
}

/// Convert a fat pointer (Box<dyn Trait>) to two u32 values for passing via JS.
fn fat_ptr_to_parts<T: ?Sized>(boxed: Box<T>) -> [u32; 2] {
    let fat_ptr: *mut T = Box::into_raw(boxed);
    unsafe { mem::transmute::<*mut T, [u32; 2]>(fat_ptr) }
}

/// Convert two u32 values back to a fat pointer (Box<dyn Trait>).
unsafe fn parts_to_fat_ptr<T: ?Sized>(parts: [u32; 2]) -> Box<T> {
    let fat_ptr = mem::transmute::<[u32; 2], *mut T>(parts);
    Box::from_raw(fat_ptr)
}

/// Wrapper type for web worker IDs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WorkerId(pub u32);

/// Status of a web worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerStatus {
    /// Spawned but haven't yet received finish init message.
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
pub struct WebWorkerManager {
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
}

thread_local! {
    static WEB_WORKER_MANAGER: RefCell<Option<WebWorkerManager>> = const { RefCell::new(None) };
}

/// Initialize the web worker manager. Must be called from main thread.
/// Safe to call multiple times (will be no-op if already initialized).
pub fn init_web_worker_manager() {
    assert_main_thread("init_web_worker_manager");
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

/// Spawn a new web worker. Returns the worker ID immediately (status will be Initializing).
/// Must be called from main thread.
pub fn spawn_worker() -> Result<WorkerId, JsValue> {
    assert_main_thread("spawn_worker");
    with_manager_mut(|manager| {
        let worker_id = manager.allocate_worker_id();

        // Create worker options for ES module worker
        let options = WorkerOptions::new();
        options.set_type(web_sys::WorkerType::Module);

        let worker = Worker::new_with_options("./worker.js", &options)?;

        // Set up onmessage handler for this worker
        let worker_id_for_closure = worker_id;
        let onmessage_closure = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
            handle_worker_message(worker_id_for_closure, event);
        });
        worker.set_onmessage(Some(onmessage_closure.as_ref().unchecked_ref()));

        // Send loading message with module and memory
        let msg = Object::new();
        Reflect::set(&msg, &"type".into(), &"loading".into())?;
        Reflect::set(&msg, &"module".into(), &wasm_bindgen::module())?;
        Reflect::set(&msg, &"memory".into(), &wasm_bindgen::memory())?;
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

    // Check message type
    let msg_type = Reflect::get(&data, &"type".into()).ok();
    let msg_type = msg_type.as_ref().and_then(|v| v.as_string());

    match msg_type.as_deref() {
        Some("finishLoading") => {
            with_manager_mut(|manager| {
                if let Some(state) = manager.workers.get_mut(&worker_id) {
                    state.status = WorkerStatus::Normal;
                }
            });
        }
        Some("custom") => {
            let js_payload = Reflect::get(&data, &"jsPayload".into()).unwrap_or(JsValue::UNDEFINED);
            let data_ptr = Reflect::get(&data, &"rustPayloadData".into())
                .ok()
                .and_then(|v| v.as_f64())
                .map(|v| v as u32);
            let vtable_ptr = Reflect::get(&data, &"rustPayloadVtable".into())
                .ok()
                .and_then(|v| v.as_f64())
                .map(|v| v as u32);

            if let (Some(data_ptr), Some(vtable_ptr)) = (data_ptr, vtable_ptr) {
                // Reconstruct fat pointer and call the callback
                let callback: Box<MainCallback> =
                    unsafe { parts_to_fat_ptr([data_ptr, vtable_ptr]) };
                // Call the callback with a reference to the manager
                WEB_WORKER_MANAGER.with(|manager| {
                    let manager = manager.borrow();
                    let manager = manager.as_ref().expect("WebWorkerManager not initialized");
                    callback(manager, worker_id, &js_payload);
                });
                // callback is dropped here
            }
        }
        _ => {
            web_sys::console::error_1(&format!("Unknown message type from worker: {:?}", msg_type).into());
        }
    }
}

/// Send a custom message from main thread to a worker.
///
/// The `callback` will be invoked on the worker side with the `js_payload`.
/// Must be called from main thread.
pub fn send_to_worker(
    worker_id: WorkerId,
    js_payload: &JsValue,
    callback: Box<WorkerCallback>,
) -> Result<(), JsValue> {
    assert_main_thread("send_to_worker");
    with_manager(|manager| {
        let state = manager
            .workers
            .get(&worker_id)
            .ok_or_else(|| JsValue::from_str("Worker not found"))?;

        // Convert fat pointer to two u32 parts
        let [data_ptr, vtable_ptr] = fat_ptr_to_parts(callback);

        // Create the message
        let msg = Object::new();
        Reflect::set(&msg, &"type".into(), &"custom".into())?;
        Reflect::set(&msg, &"jsPayload".into(), js_payload)?;
        Reflect::set(&msg, &"rustPayloadData".into(), &JsValue::from_f64(data_ptr as f64))?;
        Reflect::set(&msg, &"rustPayloadVtable".into(), &JsValue::from_f64(vtable_ptr as f64))?;

        state.worker.post_message(&msg)?;

        Ok(())
    })
}

/// Send a custom message from worker to main thread.
///
/// The `callback` will be invoked on the main thread with the manager, worker ID, and js_payload.
/// Must be called from a worker thread.
pub fn send_to_main(
    js_payload: &JsValue,
    callback: Box<MainCallback>,
) -> Result<(), JsValue> {
    assert_worker_thread("send_to_main");
    let global = js_sys::global().unchecked_into::<DedicatedWorkerGlobalScope>();

    // Convert fat pointer to two u32 parts
    let [data_ptr, vtable_ptr] = fat_ptr_to_parts(callback);

    // Create the message
    let msg = Object::new();
    Reflect::set(&msg, &"type".into(), &"custom".into())?;
    Reflect::set(&msg, &"jsPayload".into(), js_payload)?;
    Reflect::set(&msg, &"rustPayloadData".into(), &JsValue::from_f64(data_ptr as f64))?;
    Reflect::set(&msg, &"rustPayloadVtable".into(), &JsValue::from_f64(vtable_ptr as f64))?;

    global.post_message(&msg)?;

    Ok(())
}

/// Get the status of a worker. Must be called from main thread.
pub fn get_worker_status(worker_id: WorkerId) -> Option<WorkerStatus> {
    assert_main_thread("get_worker_status");
    with_manager(|manager| manager.workers.get(&worker_id).map(|s| s.status))
}

/// Get all worker IDs. Must be called from main thread.
pub fn get_all_worker_ids() -> Vec<WorkerId> {
    assert_main_thread("get_all_worker_ids");
    with_manager(|manager| manager.workers.keys().copied().collect())
}

/// Get worker IDs with a specific status. Must be called from main thread.
pub fn get_workers_by_status(status: WorkerStatus) -> Vec<WorkerId> {
    assert_main_thread("get_workers_by_status");
    with_manager(|manager| {
        manager
            .workers
            .iter()
            .filter(|(_, s)| s.status == status)
            .map(|(id, _)| *id)
            .collect()
    })
}

/// Handler for messages received by a worker. Called from worker.js.
/// Must be called from a worker thread.
#[wasm_bindgen(js_name = workerHandleMessage)]
pub fn worker_handle_message(event: MessageEvent) {
    assert_worker_thread("worker_handle_message");
    let data = event.data();

    let msg_type = Reflect::get(&data, &"type".into()).ok();
    let msg_type = msg_type.as_ref().and_then(|v| v.as_string());

    match msg_type.as_deref() {
        Some("custom") => {
            let js_payload = Reflect::get(&data, &"jsPayload".into()).unwrap_or(JsValue::UNDEFINED);
            let data_ptr = Reflect::get(&data, &"rustPayloadData".into())
                .ok()
                .and_then(|v| v.as_f64())
                .map(|v| v as u32);
            let vtable_ptr = Reflect::get(&data, &"rustPayloadVtable".into())
                .ok()
                .and_then(|v| v.as_f64())
                .map(|v| v as u32);

            if let (Some(data_ptr), Some(vtable_ptr)) = (data_ptr, vtable_ptr) {
                // Reconstruct fat pointer and call the callback
                let callback: Box<WorkerCallback> =
                    unsafe { parts_to_fat_ptr([data_ptr, vtable_ptr]) };
                callback(&js_payload);
                // callback is dropped here
            }
        }
        Some("dynamicLink") => {
            // Not implemented yet
            web_sys::console::log_1(&"Dynamic linking not implemented yet".into());
        }
        _ => {
            web_sys::console::error_1(
                &format!("Unknown message type in worker: {:?}", msg_type).into(),
            );
        }
    }
}

/// Called by worker to notify main thread that loading is complete.
/// Must be called from a worker thread.
#[wasm_bindgen]
pub fn notify_loading_complete() -> Result<(), JsValue> {
    assert_worker_thread("notify_loading_complete");
    let global = js_sys::global().unchecked_into::<DedicatedWorkerGlobalScope>();

    let msg = Object::new();
    Reflect::set(&msg, &"type".into(), &"finishLoading".into())?;

    global.post_message(&msg)?;

    Ok(())
}
