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
dx serve --hot-patch --target wasm32-unknown-unknown --bundle web
```

TODO use package

## Web server

Command

```
dx serve --hot-patch --trace --package server_hotswap_example --verbose
```

In Linux or WSL, if it error `collect2: fatal error: cannot find ‘ld’`, then install lld (`sudo apt install lld`). [Issue](https://github.com/DioxusLabs/dioxus/issues/4872)

# Ambiguity of wording

These 3 things usually refer to the same thing:

- Hotswap
- Hot reload
- Hotpatch

But in dioxus CLI, "hot reload" means reloading whole application and lose execution state. Hotpatch means apply code change without losing execution state.

