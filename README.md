# Rust hotswap Example

Simple examples of using [subsecond](https://docs.rs/subsecond/0.7.1/subsecond/) hotswap in non-Dioxus applications.

Hotswapping require some special building and linking that `cargo` cannot do. It needs to be done by Dioxus CLI. Dioxus CLI can be used for non-Dioxus applications.

Two examples: 

- In-browser wasm hotswap
- A simple webserver with hotswap

## In-browser wasm hotswap

Hotswap wasm code in browser, without page refresh, without losing Wasm execution state. (currently only singlethreaded wasm32)

Command:

```
dx serve --hot-patch --package wasm_hotswap_example --target wasm32-unknown-unknown --bundle web
```

## Web server

Command

```
dx serve --hot-patch --package server_hotswap_example
```

(Can add `--trace --verbose` for more logging)

In Linux or WSL, if it errors `collect2: fatal error: cannot find ‘ld’`, then install lld (`sudo apt install lld`). [Issue](https://github.com/DioxusLabs/dioxus/issues/4872)

In Windows, it will stackoverflow after a hotswap. The root cause is not yet known.

---

Side note: the "hotswap", "hot reload" and "hotpatch" mostly refer to the same thing. But sometimes "hot reload" refers to reloding code and losing execution state. The "hotswap" here means applying code change while keeping executing state.


