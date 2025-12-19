
Your task is to implement a web worker manager in wasm_mt_hotswap_example/src/web_worker_manager.rs 

Background: I am doing experiments about wasm multi-threaded hotswapping with subsecond. Single-threaded wasm hotswap already works. 

Hotswapping requires loading new wasm code at runtime. But in-browser wasm multithreading has many restrictions:

- Different web workers must use seprately-created WebAssembly.Instance. They can only share memory. They cannot share tables and wasm globals.
- Some objects like OfflineCanvas and WebAssembly.Module can only be sent via JS message. It cannot be sent via linear memory (SharedArrayBuffer). (Although WebAssembly.Module can be separately compiled in each web worker I choose to not do that.)
- A web worker cannot receive message without finishing its current iteration of event loop. If the web worker is running some async runtime scheduler loop in wasm, it cannot accept new web worker messages.
- There is no web API for getting a list of all web workers. 

So to do dynamic linking I need to:

- Keep a list of web workers JS references
- Send a JS message to these web workers (message includes WebAssembly.Module) which denotes dynamic linking
- Web workers must be able to process the dynamic linking message. The web worker manager need to take control over message processing.

So I need to create a simple web worker manager that:

- Manages launching of web workers. To initialize a wasm thread web worker, main thread need to send a JS message to it containing a SharedArrayBuffer, then JS calls wasm-bindgen to load.
- Manages dynamic linking.
- Allow user code to send custom message combined with a custom callback. To send things like offline canvas, custom message must be able to carry JS objects. 

To distinguish between custom message and loading message and dynamic linking message, web worker manager wraps all JS messages into a new structure. It has `type` field. When `type` is `loding` it's loading message that has SharedArrayBuffer. When `type` is `dynamicLink` it does dynamic linking (no need to implement dynamic linking for now). When `type` is custom then `jsPayload` field carries custom JS value and `rustPayload` field contains a pointer which point to a Rust boxed function.

The web worker manager is managed by main thread.

The implementation should be simple. Don't overcomplicate.