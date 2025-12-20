import wbg_init, { __wwm_internal_worker_handle_message } from './wasm/wasm_mt_hotswap_example.js';

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

    // Set worker ID on globalThis (reserved for future use)
    if ('__wwm_web_worker_id' in data) {
      globalThis.__wwm_web_worker_id = data.__wwm_web_worker_id;
    }

    // Start initialization - set wasmInit to Promise immediately
    wasmInit = wbg_init({
      module_or_path: data.__wwm_wasm_module,
      memory: data.__wwm_wasm_memory
    }).catch(err => {
      console.error('[WWM] WASM init failed:', err);
      throw err;
    });

    // Wait for init to complete
    await wasmInit;

    // Run the init callback if provided
    if ('__wwm_callback' in data) {
      __wwm_internal_worker_handle_message(data.__wwm_callback, data.__wwm_js_payload);
    }
  } else {
    // Expecting task message
    if (!('__wwm_callback' in data)) {
      console.error('[WWM] Expected task message with __wwm_callback. This web worker is managed by web worker manager. Do not use raw web API to send message to it.', data);
      return;
    }

    // Wait for init to complete before processing task
    await wasmInit;

    // Forward to Rust handler
    __wwm_internal_worker_handle_message(data.__wwm_callback, data.__wwm_js_payload);
  }
};
