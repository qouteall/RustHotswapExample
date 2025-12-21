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

Web worker manager: `wasm_mt_hotswap_example/web_worker_manager.rs`

### Things to notice

Note that wasm-bindgen has functionality of using Rust type to hold JS value (`JsValue`). These JS proxy types only hold an integer. The actual JS value is in a JS array managed by wasm-bindgen. The Rust values can be sent across threads but JS values cannot. This is an important distinction. The `JsValue` type is neither `Send` or `Sync`. JS value has to be sent via web worker message.

https://wasm-bindgen.github.io/wasm-bindgen/contributing/design/js-objects-in-rust.html

### IDE and toolchain notes

The `wasm_mt_hotswap_example` subfolder uses a different nightly toolchain with atomics enabled (`+atomics,+bulk-memory,+mutable-globals`). 

### Important instructions

Always ask me clarifying questions before changing code.

When making code changes, prioritize simplicity and maintainability over minimal diffs. Consolidate similar logic, reduce duplication, and restructure when it improves clarity.