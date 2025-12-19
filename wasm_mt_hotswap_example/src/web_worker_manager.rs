//! Web Worker Manager for managing web workers and message passing.
//!
//! This module provides a centralized manager for web workers that:
//! - Manages spawning of web workers
//! - Handles message passing between main thread and workers
//! - Tracks worker status
//! - Supports custom messages with JS payloads and Rust callbacks

use std::cell::RefCell;
use std::collections::HashMap;

use js_sys::{Object, Reflect};
use wasm_bindgen::prelude::*;
use web_sys::{MessageEvent, Worker, WorkerOptions};

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

/// Callback type for custom messages from worker to main thread.
type MainThreadCallback = Box<dyn Fn(&WebWorkerManager, WorkerId, &JsValue)>;

/// The web worker manager, managed by main thread.
pub struct WebWorkerManager {
    workers: HashMap<WorkerId, WorkerState>,
    next_worker_id: u32,
    /// Pending callbacks for messages from workers.
    /// Key is the raw pointer value sent as rustPayload.
    pending_callbacks: HashMap<usize, MainThreadCallback>,
}

impl WebWorkerManager {
    fn new() -> Self {
        Self {
            workers: HashMap::new(),
            next_worker_id: 0,
            pending_callbacks: HashMap::new(),
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
pub fn spawn_worker() -> Result<WorkerId, JsValue> {
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

/// Handle a message received from a worker.
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
            let rust_payload = Reflect::get(&data, &"rustPayload".into())
                .ok()
                .and_then(|v| v.as_f64())
                .map(|v| v as usize);

            if let Some(ptr) = rust_payload {
                // Take the callback out and execute it
                let callback = with_manager_mut(|manager| manager.pending_callbacks.remove(&ptr));

                if let Some(callback) = callback {
                    // We need to call the callback with a reference to the manager
                    // This is tricky because we can't hold the borrow while calling
                    WEB_WORKER_MANAGER.with(|manager| {
                        let manager = manager.borrow();
                        let manager = manager.as_ref().expect("WebWorkerManager not initialized");
                        callback(manager, worker_id, &js_payload);
                    });
                }
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
pub fn send_to_worker(
    worker_id: WorkerId,
    js_payload: &JsValue,
    callback: Box<dyn Fn(&JsValue)>,
) -> Result<(), JsValue> {
    with_manager(|manager| {
        let state = manager
            .workers
            .get(&worker_id)
            .ok_or_else(|| JsValue::from_str("Worker not found"))?;

        // Box the callback and get the raw pointer
        let ptr = Box::into_raw(callback) as usize;

        // Create the message
        let msg = Object::new();
        Reflect::set(&msg, &"type".into(), &"custom".into())?;
        Reflect::set(&msg, &"jsPayload".into(), js_payload)?;
        Reflect::set(&msg, &"rustPayload".into(), &JsValue::from_f64(ptr as f64))?;

        state.worker.post_message(&msg)?;

        Ok(())
    })
}

/// Send a custom message from worker to main thread.
///
/// The `callback` will be invoked on the main thread with the manager, worker ID, and js_payload.
/// This function should be called from a worker.
pub fn send_to_main(
    js_payload: &JsValue,
    callback: Box<dyn Fn(&WebWorkerManager, WorkerId, &JsValue)>,
) -> Result<(), JsValue> {
    let global = js_sys::global().unchecked_into::<web_sys::DedicatedWorkerGlobalScope>();

    // Box the callback and get the raw pointer
    let ptr = Box::into_raw(callback) as usize;

    // Create the message
    let msg = Object::new();
    Reflect::set(&msg, &"type".into(), &"custom".into())?;
    Reflect::set(&msg, &"jsPayload".into(), js_payload)?;
    Reflect::set(&msg, &"rustPayload".into(), &JsValue::from_f64(ptr as f64))?;

    global.post_message(&msg)?;

    Ok(())
}

/// Get the status of a worker.
pub fn get_worker_status(worker_id: WorkerId) -> Option<WorkerStatus> {
    with_manager(|manager| manager.workers.get(&worker_id).map(|s| s.status))
}

/// Get all worker IDs.
pub fn get_all_worker_ids() -> Vec<WorkerId> {
    with_manager(|manager| manager.workers.keys().copied().collect())
}

/// Get worker IDs with a specific status.
pub fn get_workers_by_status(status: WorkerStatus) -> Vec<WorkerId> {
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
#[wasm_bindgen(js_name = workerHandleMessage)]
pub fn worker_handle_message(event: MessageEvent) {
    let data = event.data();

    let msg_type = Reflect::get(&data, &"type".into()).ok();
    let msg_type = msg_type.as_ref().and_then(|v| v.as_string());

    match msg_type.as_deref() {
        Some("custom") => {
            let js_payload = Reflect::get(&data, &"jsPayload".into()).unwrap_or(JsValue::UNDEFINED);
            let rust_payload = Reflect::get(&data, &"rustPayload".into())
                .ok()
                .and_then(|v| v.as_f64())
                .map(|v| v as usize);

            if let Some(ptr) = rust_payload {
                // Reconstruct and call the callback
                let callback = unsafe { Box::from_raw(ptr as *mut Box<dyn Fn(&JsValue)>) };
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
pub fn notify_loading_complete() -> Result<(), JsValue> {
    let global = js_sys::global().unchecked_into::<web_sys::DedicatedWorkerGlobalScope>();

    let msg = Object::new();
    Reflect::set(&msg, &"type".into(), &"finishLoading".into())?;

    global.post_message(&msg)?;

    Ok(())
}
