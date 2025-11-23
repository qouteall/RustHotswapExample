# Rust hotswap Example

Simple examples of using [subsecond](https://docs.rs/subsecond/0.7.1/subsecond/) hotswap in non-Dioxus projects.

Hotswapping require some "hacking" of building and linking. Currently it need to be done by Dioxus CLI. Dioxus CLI can be used for non-Dioxus applications.

Two examples: 

- In-browser wasm hotswap
- A simple webserver with hotswap

## In-browser wasm hotswap

Command

```
dx serve --hot-patch --target wasm32-unknown-unknown --bundle web
```

## Web server

Command

```
TODO
```

# Ambiguity of wording

These 3 things usually refer to the same thing:

- Hotswap
- Hot reload
- Hotpatch

But in dioxus CLI, "hot reload" means reloading whole application and lose execution state. Hotpatch means apply code change without losing execution state.

