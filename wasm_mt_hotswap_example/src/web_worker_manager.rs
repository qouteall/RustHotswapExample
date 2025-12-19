//! Web Worker Manager:
//! 
//! - Manage web worker creation in multi-threaded wasm
//! - Keep track of all web workers
//! - Manage web worker 
//!
//! Manages web worker lifecycle, message passing, and (future) dynamic linking.
//! 
//! It's only usable from main thread.
//! 
//! 

use js_sys::{Array, Function, Object, Reflect};
use std::cell::RefCell;
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use web_sys::{DedicatedWorkerGlobalScope, MessageEvent, Worker, WorkerOptions};

/// Message types for worker communication
const MSG_TYPE_LOADING: &str = "loading";
const MSG_TYPE_DYNAMIC_LINK: &str = "dynamicLink";
const MSG_TYPE_CUSTOM: &str = "custom";

/// Web Worker Manager - manages worker references and message passing.
/// JS references are stored in `globalThis.__wasm_web_worker_manager`.
#[wasm_bindgen]
pub struct WebWorkerManager {
    workers: Rc<RefCell<Vec<Worker>>>,
    next_worker_id: Rc<RefCell<u32>>,
}

#[wasm_bindgen]
impl WebWorkerManager {
    /// Create a new WebWorkerManager and store it in globalThis
    #[wasm_bindgen(constructor)]
    pub fn new() -> Result<WebWorkerManager, JsValue> {
        let manager = WebWorkerManager {
            workers: Rc::new(RefCell::new(Vec::new())),
            next_worker_id: Rc::new(RefCell::new(0)),
        };

        Ok(manager)
    }

    /// Spawn a new worker and send the loading message with SharedArrayBuffer
    #[wasm_bindgen(js_name = spawnWorker)]
    pub fn spawn_worker(&self) -> Result<u32, JsValue> {
        let options = WorkerOptions::new();
        options.set_type(web_sys::WorkerType::Module);

        let worker = Worker::new_with_options("./worker.js", &options)?;

        // Create loading message
        let msg = Object::new();
        Reflect::set(&msg, &"type".into(), &MSG_TYPE_LOADING.into())?;
        Reflect::set(&msg, &"module".into(), &wasm_bindgen::module())?;
        Reflect::set(&msg, &"memory".into(), &wasm_bindgen::memory())?;

        worker.post_message(&msg)?;

        let worker_id = *self.next_worker_id.borrow();
        *self.next_worker_id.borrow_mut() += 1;

        self.workers.borrow_mut().push(worker);

        Ok(worker_id)
    }

    /// Send a custom message to a specific worker
    /// js_payload: any JS value that needs to be transferred (e.g., OffscreenCanvas)
    /// callback: a boxed Rust function pointer
    #[wasm_bindgen(js_name = sendCustomMessage)]
    pub fn send_custom_message(
        &self,
        worker_id: u32,
        js_payload: JsValue,
        callback: u32, // pointer to Box<dyn FnOnce(JsValue) + Send>
    ) -> Result<(), JsValue> {
        let workers = self.workers.borrow();
        let worker = workers
            .get(worker_id as usize)
            .ok_or_else(|| JsValue::from_str("Worker not found"))?;

        let msg = Object::new();
        Reflect::set(&msg, &"type".into(), &MSG_TYPE_CUSTOM.into())?;
        Reflect::set(&msg, &"jsPayload".into(), &js_payload)?;
        Reflect::set(&msg, &"rustPayload".into(), &JsValue::from(callback))?;

        worker.post_message(&msg)?;

        Ok(())
    }

    /// Send a custom message with transferable objects
    #[wasm_bindgen(js_name = sendCustomMessageWithTransfer)]
    pub fn send_custom_message_with_transfer(
        &self,
        worker_id: u32,
        js_payload: JsValue,
        callback: u32,
        transferables: &Array,
    ) -> Result<(), JsValue> {
        let workers = self.workers.borrow();
        let worker = workers
            .get(worker_id as usize)
            .ok_or_else(|| JsValue::from_str("Worker not found"))?;

        let msg = Object::new();
        Reflect::set(&msg, &"type".into(), &MSG_TYPE_CUSTOM.into())?;
        Reflect::set(&msg, &"jsPayload".into(), &js_payload)?;
        Reflect::set(&msg, &"rustPayload".into(), &JsValue::from(callback))?;

        worker.post_message_with_transfer(&msg, transferables)?;

        Ok(())
    }

    /// Broadcast dynamic link message to all workers (placeholder)
    #[wasm_bindgen(js_name = broadcastDynamicLink)]
    pub fn broadcast_dynamic_link(&self, module: &JsValue) -> Result<(), JsValue> {
        let workers = self.workers.borrow();

        for worker in workers.iter() {
            let msg = Object::new();
            Reflect::set(&msg, &"type".into(), &MSG_TYPE_DYNAMIC_LINK.into())?;
            Reflect::set(&msg, &"module".into(), module)?;

            worker.post_message(&msg)?;
        }

        Ok(())
    }

    /// Get the number of managed workers
    #[wasm_bindgen(js_name = getWorkerCount)]
    pub fn get_worker_count(&self) -> u32 {
        self.workers.borrow().len() as u32
    }

    /// Get a worker by ID (for advanced use cases)
    #[wasm_bindgen(js_name = getWorker)]
    pub fn get_worker(&self, worker_id: u32) -> Result<Worker, JsValue> {
        let workers = self.workers.borrow();
        workers
            .get(worker_id as usize)
            .cloned()
            .ok_or_else(|| JsValue::from_str("Worker not found"))
    }
}

/// Callback storage for custom messages
struct CustomCallback {
    func: Box<dyn FnOnce(JsValue) + Send>,
}

/// Create a callback pointer for sending with custom messages
/// Returns a u32 pointer that should be passed to send_custom_message
#[wasm_bindgen(js_name = createCustomCallback)]
pub fn create_custom_callback(callback: Box<dyn FnOnce(JsValue) + Send>) -> u32 {
    let boxed = Box::new(CustomCallback { func: callback });
    Box::into_raw(boxed) as u32
}

/// Worker-side: Handle incoming messages based on type
/// This should be called from the worker's onmessage handler
#[wasm_bindgen(js_name = workerHandleMessage)]
pub fn worker_handle_message(event: MessageEvent) -> Result<(), JsValue> {
    let data = event.data();

    // Get message type
    let msg_type: String = Reflect::get(&data, &"type".into())?
        .as_string()
        .ok_or_else(|| JsValue::from_str("Missing message type"))?;

    match msg_type.as_str() {
        MSG_TYPE_LOADING => {
            // Loading is handled by JS before Rust is initialized
            // This branch shouldn't be reached in normal operation
            Ok(())
        }
        MSG_TYPE_DYNAMIC_LINK => {
            // Placeholder for dynamic linking
            // TODO: Implement dynamic linking logic
            web_sys::console::log_1(&"Dynamic link message received (not implemented)".into());
            Ok(())
        }
        MSG_TYPE_CUSTOM => {
            let js_payload = Reflect::get(&data, &"jsPayload".into())?;
            let rust_payload = Reflect::get(&data, &"rustPayload".into())?
                .as_f64()
                .ok_or_else(|| JsValue::from_str("Invalid rust payload"))? as u32;

            // Reconstruct and call the callback
            let callback = unsafe { Box::from_raw(rust_payload as *mut CustomCallback) };
            (callback.func)(js_payload);

            // Signal completion back to main thread
            let global = js_sys::global().unchecked_into::<DedicatedWorkerGlobalScope>();
            global.post_message(&JsValue::undefined())?;

            Ok(())
        }
        _ => Err(JsValue::from_str(&format!("Unknown message type: {}", msg_type))),
    }
}

/// Worker-side entry point for executing work (legacy support)
/// This is called for backward compatibility with the existing pool
#[wasm_bindgen(js_name = workerEntryPoint)]
pub fn worker_entry_point(ptr: u32) -> Result<(), JsValue> {
    // This is the legacy entry point from pool.rs
    // Delegate to the existing child_entry_point
    crate::pool::child_entry_point(ptr)
}
