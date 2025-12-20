import wbg_init, { __wwm_internal_worker_init, __wwm_internal_worker_handle_message } from './wasm/wasm_mt_hotswap_example.js';

// Web Worker Manager protocol implementation
// wasmInit: null = expect init message, Promise = expect task message
let wasmInit = null;

self.onmessage = async (event) => {
  const data = event.data;

  // Validate message format
  if (!data || typeof data !== 'object') {
    console.error('[WWM] Invalid message format: not an object', data);
    return;
  }

  if (wasmInit === null) {
    // Expecting initialization message
    if (!('__wwm_wasm_module' in data) || !('__wwm_wasm_memory' in data)) {
      console.error('[WWM] Expected init message. This web worker is managed by web worker manager. Do not use raw web API to send message to it.', data);
      return;
    }

    const threadId = data.__wwm_thread_id;
    const senderId = data.__wwm_sender_id;

    // Start initialization - set wasmInit to Promise immediately
    wasmInit = wbg_init({
      module_or_path: data.__wwm_wasm_module,
      memory: data.__wwm_wasm_memory
    }).catch(err => {
      console.error('[WWM] WASM init failed:', err);
      throw err;
    });

    // Wait for WASM init to complete
    await wasmInit;

    // Initialize worker thread state with MessagePorts
    // ports[tid] = port to communicate with ThreadId(tid)
    const ports = data.__wwm_ports || [];
    const threadCount = data.__wwm_thread_count || 0;

    __wwm_internal_worker_init(threadId, ports, threadCount);

    // Run the init callback if provided
    if ('__wwm_callback' in data) {
      __wwm_internal_worker_handle_message(senderId, data.__wwm_callback, data.__wwm_js_payload);
    }
  } else {
    // Expecting task message
    if (!('__wwm_callback' in data)) {
      console.error('[WWM] Expected task message with __wwm_callback. This web worker is managed by web worker manager. Do not use raw web API to send message to it.', data);
      return;
    }

    // Wait for init to complete before processing task
    await wasmInit;

    const senderId = data.__wwm_sender_id || 0;

    // Forward to Rust handler
    __wwm_internal_worker_handle_message(senderId, data.__wwm_callback, data.__wwm_js_payload);
  }
};
