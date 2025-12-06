# Rust hotswap Example

Simple examples of using [subsecond](https://docs.rs/subsecond/0.7.1/subsecond/) hotswap in non-Dioxus applications.

Hotswapping require some special building and linking that `cargo` cannot do. It needs to be done by Dioxus CLI. Dioxus CLI can be used for non-Dioxus applications.

In https://github.com/DioxusLabs/dioxus/releases/tag/v0.7.0 :

> The infrastructure to support Subsecond is quite complex. Currently, we plan to only ship the Subsecond engine within the Dioxus CLI itself with a long-term plan to spin the engine out into its own crate. For now, we still want the ecosystem to experience the magic of Subsecond, so we’ve made the CLI compatible with non-dioxus projects and removed “dioxus” branding when not serving a dioxus project.

Two examples: 

- In-browser wasm hotswap
- A simple webserver with hotswap

## In-browser wasm hotswap

Hotswap wasm code in browser, without page refresh, without losing Wasm execution state. (currently only singlethreaded wasm32)

Command:

```
dx serve --hot-patch --package wasm_hotswap_example --target wasm32-unknown-unknown --bundle web
```

On Windows, it currently cannot build due to a bug in Dioxus CLI. Fixed in [this PR](https://github.com/DioxusLabs/dioxus/pull/5010) (not yet merged).

The `index.html` will be used by Dioxus CLI for serving web page.

## Web server

Command

```
dx serve --hot-patch --package server_hotswap_example
```

(Can add `--trace --verbose` for more logging)

In Linux or WSL, if it errors `collect2: fatal error: cannot find ‘ld’`, install lld (`sudo apt install lld`). [Issue](https://github.com/DioxusLabs/dioxus/issues/4872)

In Windows, it will stackoverflow after a hotswap. The root cause is not yet known.

It will deadloop if ran without connecting to dev server (https://github.com/DioxusLabs/dioxus/issues/4305#issuecomment-3585614449).

Note that it will restart internal axum server after hotswap. It cannot keep long connections (e.g. websocket of webserver) after hotpatch. It can be solved by changing the hotswap boundary (then hotpatch cannot add or remove Restful APIs). TODO

## In-borwser multi-threaded wasm hotswap (work-in-progress)

Changed from this example https://github.com/wasm-bindgen/wasm-bindgen/tree/main/examples/raytrace-parallel

I changed it to use ES module. (Firefox now supports ES module.)

It uses nightly Rust.

How to build and run without dx hotswap, without wasm-pack:

Go into `wasm_mt_hotswap_example` folder

```
cargo build --target wasm32-unknown-unknown "-Zbuild-std=std,panic_abort" --config "target.wasm32-unknown-unknown.rustflags='-Ctarget-feature=+atomics -Clink-args=--shared-memory -Clink-args=--max-memory=1073741824 -Clink-args=--import-memory -Clink-args=--export=__wasm_init_tls -Clink-args=--export=__tls_size -Clink-args=--export=__tls_align -Clink-args=--export=__tls_base'"

wasm-bindgen ../target/wasm32-unknown-unknown/debug/wasm_mt_hotswap_example.wasm --out-dir ../dist/raytrace-parallel --typescript --target web
```

Then copy files in `wasm_mt_hotswap_example/public/*` and `wasm_mt_hotswap/index.html` into `dist/raytrace-parallel` folder.

In VSCode, install live preview plugin, open `dist/raytrace-parallel/index.html` in VSCode, click show-preview on right top corner, then click the icon on the right side of address bar, click "open in browser".

Note that in `.vscode/settings.json` it adds necessary header for site isolation, which is necessary for `SharedArrayBuffer`, which is necessary for wasm multi-threading.

Hotswap with dx (not yet working):

Go to `wasm_mt_hotswap_example` folder

```
dx serve --hot-patch --target wasm32-unknown-unknown --bundle web --cargo-args " -Zbuild-std=std,panic_abort"
```

---

Side note: the "hotswap", "hot reload" and "hotpatch" mostly refer to the same thing. But sometimes "hot reload" refers to reloding code and losing execution state. The "hotswap" here means applying code change while keeping executing state.


