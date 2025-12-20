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

Web worker manager aims to workaround previous constraints.

- Manage web workers and track their JS references in main thread.
- Take control over web worker initialization, message passing, message processing.
- Allow dynamic linking. Due to the previous constraints, to do dynamic linking, each web worker need to cooperatively load new wasm module and change their own table. And that can only be done if web worker finishes processing current message (cannot stuck in scheduler loop).
- Make it easier to send a Rust closure combined with JS value to any thread.
- Support worker-to-worker direct communication via MessageChannel.

#### Thread IDs

- `ThreadId(0)` = main thread
- `ThreadId(1..=n)` = worker threads

The type is `ThreadId` (not `WorkerId`). All threads know their own ID via `my_thread_id()`.

#### Initialization

The web worker manager uses fixed worker count at initialization:

```
init_web_worker_manager(worker_count, on_init_callback)?;
```

During init:
1. Creates all workers upfront
2. Creates n² MessageChannels for worker-to-worker communication
3. Sends init message to each worker with WASM module, memory, and MessagePorts

#### Message protocol

The web worker manager takes over JS message passing between all threads.

**Init message (main → worker):**
```
{
  __wwm_wasm_module: WebAssembly.Module,
  __wwm_wasm_memory: WebAssembly.Memory,
  __wwm_callback: [dataPtr, vTablePtr],  // fat pointer for Box<dyn FnOnce(ThreadId, JsValue) + Send>
  __wwm_js_payload: any,                 // user-defined payload
  __wwm_thread_id: number,               // this worker's ThreadId (1..=n)
  __wwm_sender_id: number,               // sender's ThreadId (always 0 for init)
  __wwm_outbound_ports: [MessagePort | null, ...],  // ports to send to other workers
  __wwm_inbound_ports: [MessagePort | null, ...],   // ports to receive from other workers
  __wwm_worker_count: number             // total number of workers
}
```

Port arrays are indexed by worker index (0-based, corresponding to ThreadId 1..=n).
`outbound_ports[i]` is the port to send to worker `i+1`.
`inbound_ports[i]` is the port to receive from worker `i+1`.
Self-referencing entries are `null`.

**Task message (any direction - main↔worker or worker↔worker):**
```
{
  __wwm_callback: [dataPtr, vTablePtr],  // fat pointer for Box<dyn FnOnce(ThreadId, JsValue) + Send>
  __wwm_js_payload: any,                 // user-defined payload
  __wwm_sender_id: number                // sender's ThreadId
}
```

The callback fat pointer is heap-allocated by the sender and dropped by the receiver after invocation.

#### Unified send API

```rust
send_to_thread(target: ThreadId, callback: Box<dyn FnOnce(ThreadId, JsValue) + Send>, js_payload: &JsValue)
```

Routing:
- To main thread (ThreadId(0)): uses `self.postMessage()` from worker
- From main thread to worker: uses `Worker.postMessage()`
- From worker to worker: uses MessageChannel

#### Worker-to-worker communication

Each worker receives MessagePort arrays during init:
- `outbound_ports`: ports to send to other workers
- `inbound_ports`: ports to receive from other workers

This enables direct worker-to-worker messaging without going through main thread.

#### Web worker tracking

Main thread tracks worker status:
- Initializing: Spawned but init callback hasn't completed yet
- Normal: Init callback completed
- DynamicLinking: (reserved for future)
- Finalizing: (reserved for future)

#### Thread-local storage

Main thread stores:
- Worker handles and status in `MAIN_THREAD_STATE`

Workers store:
- Own ThreadId in `MY_THREAD_ID`
- Outbound MessagePorts in `WORKER_THREAD_STATE`

#### JS worker.js protocol

In web worker JS code, it has a `wasmInit` global (nullable Promise):
- If null: expects init message only
- If not null: expects task message only
- Task message processing awaits the init promise

User code cannot change `onmessage` using raw web API or send messages using raw web API.

#### Important things

Note that wasm-bindgen has functionality of using Rust type to hold JS value (`JsValue`). These JS proxy types only hold an integer. The actual JS value is in a JS array managed by wasm-bindgen. The Rust values can be sent across threads but JS values cannot. This is an important distinction. The `JsValue` type is neither `Send` or `Sync`. JS value has to be sent via web worker message.

https://wasm-bindgen.github.io/wasm-bindgen/contributing/design/js-objects-in-rust.html

