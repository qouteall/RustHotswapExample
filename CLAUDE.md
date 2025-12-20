# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Overview

This repository contains examples of using [subsecond](https://docs.rs/subsecond/0.7.1/subsecond/) hotswap in non-Dioxus applications. Hotswapping allows applying code changes while keeping execution state (no page refresh, no state loss).

## Build Commands

See README.md

## Current work focus

The multithreaded wasm hotswap is being worked on. It's in `wasm_mt_hotswap_example` folder.

It was changed from the parallel raytrace example (https://github.com/wasm-bindgen/wasm-bindgen/tree/main/examples/raytrace-parallel).

Don't care about `pool.rs` or copy its design. I am going to rework `pool.rs`.

## Architecture

### Workspace Structure
- `wasm_hotswap_example/` - Single-threaded browser WASM hotswap demo
- `server_hotswap_example/` - Axum web server with hotswap
- `wasm_mt_hotswap_example/` - Multi-threaded browser WASM hotswap (work-in-progress)

### Multi-threaded WASM hotswap (`wasm_mt_hotswap_example`)

It's not yet working. It's being worked on.

#### Web multi-threading constraints

In-browser WASM multi-threading has restrictions:

- Different web workers must use separately-created `WebAssembly.Instance` (can only share memory, not tables/globals)
- Objects like `OffscreenCanvas` and `WebAssembly.Module` can only be sent via JS message, not SharedArrayBuffer
- Web workers cannot receive messages while running WASM event loops
- There is no Web API for getting a list of all web workers

#### Purpose of web worker manager

Web worker manager aim to workaround previous constraints.

- Manage web workers and track their JS references in main thread.
- Take control over web worker initialization, message passing, message processing.
- Allow dynamic linking. Due to the previous constraints, to do dynamic linking, each web worker need to cooperatively load new wasm module and change their own table. And that can only be done if web worker finishes processing current message (cannot stuck in scheduler loop).
- Make it easier to send a Rust closure combined with JS value to web worker to run, and the reverse.

#### Message protocol

The web worker manager take over the JS message passing between web worker and main thread.

For the message from main thread to web worker, there are two kinds:

- Initialization message. It's a JS object containing field
  - `__wwm_wasm_module` in type `WebAssembly.Module`
  - `__wwm_wasm_memory` in type `WebAssembly.Memory`
  - `__wwm_callback` is a JS array that contains two numbers, corresponding to fat pointer `Box<dyn FnOnce(JSValue) + Send>`. It's the callback that immediately runs after wasm initialized. (https://doc.rust-lang.org/reference/types/trait-object.html https://doc.rust-lang.org/nomicon/exotic-sizes.html#dynamically-sized-types-dsts)
  - `__wwm_js_payload` a JS value as argument to init func. (for passing things that cannot be sent via linear memory e.g. `WebAssembly.Module` `OffscreenCanvas`)
  - `__wwm_web_worker_id` a number corresponding to web worker ID. Will be set as web worker's `globalThis.__wwm_web_worker_id` (not yet used, reserved for future)
- Task message. It's a JS object containing field
  - `__wwm_callback` is a JS array that contains two numbers, corresponding to fat pointer `Box<dyn FnOnce(JSValue) + Send>` like the above.
  - `__wwm_js_payload` a JS value as argument to callback

The task message is versatile. It can do dynamic linking or user-defined custom work or other things.

In web worker JS code, it has a `wasmInit` global. Its type is nullable Promise. If it's null, then it must only receive init message (receiving wrong format message errors). If it's not null, then it must only receive task message (receiving wrong format message errors). Processing of task message awaits the promise.

For the message from web worker to main thread, only one kind: task message. JS object containing these fields:
- `__wwm_callback` is a JS array that contains two numbers, corresponding to fat pointer `Box<dyn FnOnce(JSValue) + Send>` like the above.
- `__wwm_js_payload` a JS value as argument to callback.

When using web worker manager, the user code cannot change `onmessage` using raw web API or send message using raw web API. The JS message processing should check for fields (including `__wwm_callback`) and give error when an unknown message is seen.

The fat pointer Box is heap-allocated by caller. It's auto-dropped by other side processing the message.

#### Web worker tracking

The web worker manager is managed by main thread. 

Use auto-increment u32 as web worker ID. Wrapped as type `WorkerId`. Only main thread know worker ID. When receiving message from worker, worker id comes from different callbacks set to different workers.

It also needs to track web worker status. The statuses:

- Initializing. Spawned but haven't yet received finish init message. (we can send task message to a web worker in initializing status, it will use browser's web worker event queue)
- Normal. The finish init message has been received.
- DynamicLinking. (No need to implement dynamic linking for now.)
- Finalizing. (For future graceful exiting. No need to implement now.)

After spawning a web worker, main thread immediately sends init message. The init message contains a callback that runs in web worker. In the callback, the web worker send a message to main thread to update its status from Initializing to Normal.

The JS closure set to web worker `onmessage` in main thread contains web worker ID so that main thread know which web worker.

In web worker JS, as initialization is async, it needs to await init promise before handling task message.

The web API allows web worker to manage its sub-workers. But the web worker manager don't allow that for simplicity.

#### Important things

Note that wasm-bindgen has functionality of using Rust type to hold JS value. These JS proxy types only hold an integer. The actual JS value is in a JS array managed by wasm-bindgen. The Rust values can be sent across threads but JS values cannot. This is an important distinction. Sending wasm-bindgen JS proxy types across threads is wrong. JS value has to be sent via web worker message.

https://wasm-bindgen.github.io/wasm-bindgen/contributing/design/js-objects-in-rust.html

