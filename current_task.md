
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
- Web workers must be able to process the dynamic linking message. The web worker manager need to take control over message processing (the JS callback is managed by web worker manager. both from main thread to web worker and reverse).

So I need to create a simple web worker manager that:

- Manages launching of web workers. To initialize a wasm thread web worker, main thread need to send a JS message to it containing a SharedArrayBuffer, then JS calls wasm-bindgen to load.
- Manages dynamic linking.
- Allow user code to send custom message combined with a custom callback. To send things like offline canvas, custom message must be able to carry JS objects. 

For the message from main thread to web worker:

To distinguish between custom message and loading message and dynamic linking message, web worker manager wraps all JS messages into a new structure. It has `type` field. 

- When `type` is `loading` it's loading message that has SharedArrayBuffer.
- When `type` is `dynamicLink` it does dynamic linking (no need to implement dynamic linking for now). 
- When `type` is `custom` then `jsPayload` field carries custom JS value and `rustPayload` field contains a pointer which point to a Rust boxed function. The outer API of sending message accepts a `Box<dyn Fn(&JSValue)>` and that function will be invoked in web worker.

The web worker's `onmessage` is initialized in `worker.js` and won't change after initializing. Note that loading wasm is async so it should await loading before processing new message.

For the message from web worker to main thread:

- When `type` is `finishLoading` it tells main thread web worker init is done.
- When `type` is `custom` then it has `jsPayload` and `rustPayload` fields. The outer API of sending message accepts `Box<dyn Fn(&WebWorkerManager, WorkerId, &JSValue)>`

Note that wasm-bindgen has functionality of using Rust type to hold JS value. The actual JS value is in a JS array managed by wasm-bindgen. The Rust values can be sent across threads but JS values cannot. This is an important distinction. Sending wasm-bindgen JS proxy types across threads is wrong. JS value has to be sent via web worker message.

The web worker manager is managed by main thread. It's a global value that's lazily-initialized. Its internal data structure can use `RefCell`. Its APIs are global functions (not methods). Its APIs should check whether current web worker supports the operation.

Use auto-increment u32 as web worker ID. Wrapped as type `WorkerId`. Only main thread know worker ID. When receiving message from worker, worker id comes from different callbacks set to different workers.

It also need to track web worker status. The statuses:

- Initializing. Spawned but haven't yet received finish init message.
- Normal. The finish init message has been received.
- DynamicLinking. (No need to implement dynamic linking for now.)
- Finalizing. (For future graceful exiting. No need to implement now.)

Don't care about `pool.rs` or copy its design. I am going to rework `pool.rs`.