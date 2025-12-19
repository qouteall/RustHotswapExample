import wbg_init, { child_entry_point, workerHandleMessage, notify_loading_complete } from './wasm/wasm_mt_hotswap_example.js';

// Unified message handler for web worker manager
// Messages are wrapped with { type, ... } structure
let initialised = null;

self.onmessage = async (event) => {
  const data = event.data;

  // Check if this is a wrapped message (has 'type' field)
  if (data && typeof data === 'object' && 'type' in data) {
    switch (data.type) {
      case 'loading':
        // Initialize wasm-bindgen with module and memory
        initialised = wbg_init({
          module_or_path: data.module,
          memory: data.memory
        }).catch(err => {
          setTimeout(() => { throw err; });
          throw err;
        });
        // Wait for init to complete, then notify main thread
        await initialised;
        notify_loading_complete();
        break;

      case 'dynamicLink':
        // Wait for initialization before handling dynamic link
        if (initialised) {
          await initialised;
        }
        // Forward to Rust handler
        workerHandleMessage(event);
        break;

      case 'custom':
        // Wait for initialization before handling custom message
        if (initialised) {
          await initialised;
        }
        // Forward to Rust handler
        workerHandleMessage(event);
        break;

      default:
        console.error('Unknown message type:', data.type);
    }
  } else if (Array.isArray(data) && data.length === 2) {
    // Legacy format: [module, memory] for backward compatibility with pool.rs
    let [module, memory] = data;
    initialised = wbg_init({
      module_or_path: module,
      memory: memory
    }).catch(err => {
      setTimeout(() => { throw err; });
      throw err;
    });
  } else if (typeof data === 'number') {
    // Legacy format: pointer for child_entry_point (pool.rs compatibility)
    if (initialised) {
      await initialised;
    }
    child_entry_point(data);
  } else {
    console.error('Unrecognized message format:', data);
  }
};
