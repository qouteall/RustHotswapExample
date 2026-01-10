use anyhow::{bail, Context};
use dioxus_devtools::DevserverMsg;
use futures_channel::oneshot;
use js_sys::WebAssembly::Module;
use js_sys::{ArrayBuffer, JsString, Reflect, Uint8Array};
use js_sys::{
    Object, Promise, SharedArrayBuffer, Uint8ClampedArray,
    WebAssembly::{self, Memory, Table},
};
use manganis::{asset, Asset};
use rayon::prelude::*;
use std::{io, mem};
use std::io::Read;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};
use std::sync::{Arc, RwLock};
use subsecond::{JumpTable, PatchError};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{console, ImageData};
use web_sys::{MessageEvent, WebSocket};

use crate::pool::{broadcast_to_workers, pool_get_web_worker_num, submit_to_pool};

static TEST_CSS_ASSET: Asset = asset!("/assets/test.css");

fn main() {
    // this is just placeholder
    // it won't be called when a #[wasm_bindgen(start)] function exists, because
    // https://github.com/DioxusLabs/dioxus/blob/f7e102a0b4868f51f35059ddacb19d78f10f0fa6/packages/cli/src/build/request.rs#L4242
    // dioxus doesn't work with lib target, so we need to pretend this lib is a bin
}

macro_rules! console_log {
    ($($t:tt)*) => (crate::log(&format_args!($($t)*).to_string()))
}

mod pool;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console)]
    fn log(s: &str);
    #[wasm_bindgen(js_namespace = console, js_name = log)]
    fn logv(x: &JsValue);
}

#[wasm_bindgen]
pub struct Scene {
    inner: raytracer::scene::Scene,
}

#[wasm_bindgen]
impl Scene {
    /// Creates a new scene from the JSON description in `object`, which we
    /// deserialize here into an actual scene.
    #[wasm_bindgen(constructor)]
    pub fn new(object: JsValue) -> Result<Scene, JsValue> {
        Ok(Scene {
            inner: serde_wasm_bindgen::from_value(object)
                .map_err(|e| JsValue::from(e.to_string()))?,
        })
    }

    /// Renders this scene with the provided concurrency and worker pool.
    ///
    /// This will spawn up to `concurrency` workers which are loaded from or
    /// spawned into `pool`. The `RenderingScene` state contains information to
    /// get notifications when the render has completed.
    pub fn render(self, concurrency: usize) -> Result<RenderingScene, JsValue> {
        let scene = self.inner;
        let height = scene.height;
        let width = scene.width;

        // Allocate the pixel data which our threads will be writing into.
        let pixels = (width * height) as usize;
        let mut rgb_data = vec![0; 4 * pixels];
        let base = rgb_data.as_ptr() as usize;
        let len = rgb_data.len();

        // Configure a rayon thread pool which will pull web workers from
        // `pool`.
        let thread_pool = rayon::ThreadPoolBuilder::new()
            .num_threads(concurrency)
            .spawn_handler(|thread| {
                submit_to_pool(|| thread.run());
                // Update: seems that it must spawn new threads, cannot queue task
                // otherwise parallelism is not enough, rayon will stuck inside
                Ok(())
            })
            .build()
            .unwrap();

        // And now execute the render! The entire render happens on our worker
        // threads so we don't lock up the main thread, so we ship off a thread
        // which actually does the whole rayon business. When our returned
        // future is resolved we can pull out the final version of the image.
        let (tx, rx) = oneshot::channel();
        submit_to_pool(move || {
            thread_pool.install(|| {
                subsecond::call(|| {
                    rgb_data
                        .par_chunks_mut(4)
                        .enumerate()
                        .for_each(|(i, chunk)| {
                            let i = i as u32;
                            let x = i % width;
                            let y = i / width;
                            let ray = raytracer::Ray::create_prime(x, y, &scene);
                            let result = raytracer::cast_ray(&scene, &ray, 0).to_rgba();
                            // chunk[0] = result.data[0];
                            chunk[0] = 255u8;
                            chunk[1] = result.data[1];
                            chunk[2] = result.data[2];
                            chunk[3] = result.data[3];
                        });
                })
            });
            drop(tx.send(rgb_data));
        })?;

        let done = async move {
            match rx.await {
                Ok(_data) => Ok(image_data(base, len, width, height).into()),
                Err(_) => Err(JsValue::undefined()),
            }
        };

        Ok(RenderingScene {
            promise: wasm_bindgen_futures::future_to_promise(done),
            base,
            len,
            height,
            width,
        })
    }
}

#[wasm_bindgen]
pub struct RenderingScene {
    base: usize,
    len: usize,
    promise: Promise,
    width: u32,
    height: u32,
}

#[wasm_bindgen]
impl RenderingScene {
    /// Returns the JS promise object which resolves when the render is complete
    pub fn promise(&self) -> Promise {
        self.promise.clone()
    }

    /// Return a progressive rendering of the image so far
    #[wasm_bindgen(js_name = imageSoFar)]
    pub fn image_so_far(&self) -> ImageData {
        image_data(self.base, self.len, self.width, self.height)
    }
}

fn image_data(base: usize, len: usize, width: u32, height: u32) -> ImageData {
    // Use the raw access available through `memory.buffer`, but be sure to
    // use `slice` instead of `subarray` to create a copy that isn't backed
    // by `SharedArrayBuffer`. Currently `ImageData` rejects a view of
    // `Uint8ClampedArray` that's backed by a shared buffer.
    //
    // FIXME: that this may or may not be UB based on Rust's rules. For example
    // threads may be doing unsynchronized writes to pixel data as we read it
    // off here. In the context of Wasm this may or may not be UB, we're
    // unclear! In any case for now it seems to work and produces a nifty
    // progressive rendering. A more production-ready application may prefer to
    // instead use some form of signaling here to request an update from the
    // workers instead of synchronously acquiring an update, and that way we
    // could ensure that even on the Rust side of things it's not UB.
    let mem = wasm_bindgen::memory().unchecked_into::<WebAssembly::Memory>();
    let mem = Uint8ClampedArray::new(&mem.buffer()).slice(base as u32, (base + len) as u32);
    ImageData::new_with_js_u8_clamped_array_and_sh(&mem, width, height).unwrap()
}

#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();

    init_hotpatch();

    console::log_1(&"Hello world from Rust WASM!".into());
}

#[cfg(not(debug_assertions))]
fn init_hotpatch(on_hotpatch_callback: Box<dyn Fn()>) {
    // empty in release
}

// https://github.com/DioxusLabs/dioxus/blob/main/packages/web/src/devtools.rs
#[cfg(debug_assertions)]
fn init_hotpatch() {
    web_sys::console::info_1(&format!("Initializing hotpatch").into());

    // Get the location of the devserver, using the current location plus the /_dioxus path
    // The idea here being that the devserver is always located on the /_dioxus behind a proxy

    let location = web_sys::window().unwrap().location();
    let url = format!(
        "{protocol}//{host}/_dioxus?build_id={build_id}",
        protocol = match location.protocol().unwrap() {
            prot if prot == "https:" => "wss:",
            _ => "ws:",
        },
        host = location.host().unwrap(),
        build_id = dioxus_cli_config::build_id(),
    );

    let ws = WebSocket::new(&url).unwrap();

    ws.set_onmessage(Some(
        Closure::<dyn FnMut(MessageEvent)>::new(move |e: MessageEvent| {
            let Ok(text) = e.data().dyn_into::<JsString>() else {
                return;
            };

            // The devserver messages have some &'static strs in them, so we need to leak the source string
            let string: String = text.into();
            let string = Box::leak(string.into_boxed_str());

            match serde_json::from_str::<DevserverMsg>(string) {
                Ok(DevserverMsg::HotReload(hr)) => {
                    if let Some(jumptable) = hr.clone().jump_table {
                        wasm_bindgen_futures::spawn_local(async move {
                            unsafe {
                                let (applier, module) =
                                    wasm_multi_threaded_hotpatch_apply_begin(
                                        jumptable,
                                        pool_get_web_worker_num() as u32,
                                    ).await.unwrap();

                                let applier = Arc::new(applier);

                                broadcast_to_workers(
                                    Arc::new(move |_| {
                                        let applier2 = applier.clone();
                                        wasm_bindgen_futures::spawn_local(async move {
                                            applier2.dynamic_link_in_existing_web_worker().await.unwrap();
                                        });
                                    }),
                                    JsValue::undefined()
                                )
                                    .unwrap();
                            }
                        });
                    }
                }

                Ok(DevserverMsg::Shutdown) => {
                    web_sys::console::error_1(&"Connection to the devserver was closed".into())
                }

                Err(e) => web_sys::console::error_1(
                    &format!("Error parsing devserver message: {}", e).into(),
                ),

                Ok(e) => {
                    web_sys::console::info_1(&format!("Ignore devserver message: {:?}", e).into());
                }
            }
        })
        .into_js_value()
        .as_ref()
        .unchecked_ref(),
    ));

    console::log_1(&"Hotpatch initialized".into());
}

#[cfg(target_arch = "wasm32")]
pub struct WasmMultiThreadedHotPatchApplier {
    jump_table: JumpTable,
    table_base: u64,
    memory_base: u64,
    pending_web_worker_count: AtomicI32,
}

/// In WebAssembly multi-threading, applying patch cannot be done in one-shot function call.
/// Because currently the Wasm function table cannot be shared across threads.
/// Any dynamic linking requires each thread to cooperatively create new WebAssembly instance,
/// and apply changes to their own function table.
/// We must only change global jump table after all threads have dynamically linked the new code.
///
/// One-shot hotpatch in Wasm multithreading is possible after shared-everything-threads proposal,
/// which is still in early stage. https://github.com/WebAssembly/shared-everything-threads
pub async unsafe fn wasm_multi_threaded_hotpatch_apply_begin(
    mut jump_table: JumpTable,
    pending_web_worker_count: u32,
) -> Result<(WasmMultiThreadedHotPatchApplier, Module), PatchError> {
    let funcs: Table = wasm_bindgen::function_table().unchecked_into();
    let table_base = funcs.length();

    // the function addresses are relative. add them with table base to become absolute
    // in Wasm, function address means offset into function table
    for v in jump_table.map.values_mut() {
        *v += table_base as u64;
    }

    let module = load_wasm_module(&mut jump_table).await;

    let dylink_section_info = parse_dylink_section(&module).expect("Cannot parse dylink.0 section");

    console_log!("Patch binary data size {}", dylink_section_info.mem_info.memory_size);

    const PAGE_SIZE: u32 = 64 * 1024;
    let page_count = dylink_section_info.mem_info.memory_size.div_ceil(PAGE_SIZE);
    let memory_base = (page_count + 1) * PAGE_SIZE;

    let memory: Memory = wasm_bindgen::memory().unchecked_into();
    memory.grow(page_count);

    let applier = WasmMultiThreadedHotPatchApplier {
        jump_table,
        table_base: table_base as u64,
        memory_base: memory_base as u64,
        pending_web_worker_count: AtomicI32::new(pending_web_worker_count as i32),
    };

    applier.internal_per_thread_dynamic_link(&module).await;

    Ok((applier, module))
}

#[cfg(target_arch = "wasm32")]
impl WasmMultiThreadedHotPatchApplier {
    pub async unsafe fn dynamic_link_in_existing_web_worker(&self) -> Result<(Module, bool), PatchError> {
        // each web worker will repeatedly fetch and compile Wasm module
        // V8 has a caching mechanism so it will probably not waste performance
        // https://v8.dev/blog/wasm-code-caching
        let module = load_wasm_module(&self.jump_table).await;

        self.internal_per_thread_dynamic_link(&module).await;

        let prev_pending_web_worker_num = self.pending_web_worker_count.fetch_sub(1, Ordering::SeqCst);

        if prev_pending_web_worker_num < 1 {
            panic!("`dynamic_link_in_existing_web_worker` called too many times.")
        }

        let done = if prev_pending_web_worker_num == 1 {
            self.apply_change_to_jump_table();

            true
        } else {
            false
        };

        Ok((module, done))
    }

    unsafe fn apply_change_to_jump_table(&self) {
        unsafe { subsecond::commit_patch(self.jump_table.clone()) };
    }

    pub async unsafe fn on_new_web_worker_initialize(&self) -> Result<Module, PatchError> {
        let module = load_wasm_module(&self.jump_table).await;

        self.internal_per_thread_dynamic_link(&module).await;

        Ok(module)
    }

    async unsafe fn internal_per_thread_dynamic_link(&self, wasm_module: &Module) {
        let funcs: Table = wasm_bindgen::function_table().into();
        let exports: Object = wasm_bindgen::exports().into();

        let old_table_size = funcs.length();
        assert_eq!(
            old_table_size as u64,
            self.table_base,
            "The current threads' table size doesn't correspond to table_base. \
            Maybe due to \
            1. some race condition related to spawning new web worker during hotpatch\
            2. unexpectedly doing multiple hotpatches concurrently\
            3. new web worker doesn't do dynamic linking to previous patches correctly\
            4. other possible errors"
        );

        // We grow the ifunc table to accommodate the new functions
        // In theory we could just put all the ifuncs in the jump map and use that for our count,
        // but there's no guarantee from the jump table that it references "itself"
        // We might need a sentinel value for each ifunc in the jump map to indicate that it is
        funcs
            .grow(self.jump_table.ifunc_count as u32)
            .expect("growing table");

        // Build up the import object. We copy everything over from the current exports, but then
        // need to add in the memory and table base offsets for the relocations to work.
        //
        // let imports = {
        //     env: {
        //         memory: base.memory,
        //         __tls_base: base.__tls_base,
        //         __stack_pointer: base.__stack_pointer,
        //         __indirect_function_table: base.__indirect_function_table,
        //         __memory_base: memory_base,
        //         __table_base: table_base,
        //        ..base_exports
        //     },
        // };
        let env = Object::new();

        // Move memory, __tls_base, __stack_pointer, __indirect_function_table, and all exports over
        for key in Object::keys(&exports) {
            Reflect::set(
                &env,
                &key,
                &Reflect::get(&exports, &key).expect("getting field from exports"),
            )
                .expect("setting env");
        }

        // Set the memory and table in the imports
        // Following this pattern: Global.new({ value: "i32", mutable: false }, value)
        for (name, value) in [("__table_base", self.table_base), ("__memory_base", self.memory_base)] {
            let descriptor = Object::new();
            Reflect::set(&descriptor, &"value".into(), &"i32".into()).expect("setting descriptor");
            Reflect::set(&descriptor, &"mutable".into(), &false.into()).expect("setting descriptor2");
            let value = WebAssembly::Global::new(&descriptor, &value.into()).expect("new global");
            Reflect::set(&env, &name.into(), &value.into()).expect("setting env global");
        }

        // Set the memory and table in the imports
        let imports = Object::new();
        Reflect::set(&imports, &"env".into(), &env).expect("setting env into imports");

        let instance = JsFuture::from(WebAssembly::instantiate_module(wasm_module, &imports))
            .await
            .expect("instantiating module");

        console::log_2(&"result instance".into(), &instance);

        let exports: Object = Reflect::get(&instance, &"exports".into())
            .expect("getting exports")
            .unchecked_into();

        // https://github.com/WebAssembly/tool-conventions/blob/main/DynamicLinking.md#relocations
        _ = Reflect::get(&exports, &"__wasm_apply_data_relocs".into())
            .unwrap()
            .unchecked_into::<js_sys::Function>()
            .call0(&JsValue::undefined());
        _ = Reflect::get(&exports, &"__wasm_apply_global_relocs".into())
            .unwrap()
            .unchecked_into::<js_sys::Function>()
            .call0(&JsValue::undefined());

        // https://github.com/WebAssembly/tool-conventions/blob/main/Linking.md#start-section
        _ = Reflect::get(&exports, &"__wasm_call_ctors".into())
            .unwrap()
            .unchecked_into::<js_sys::Function>()
            .call0(&JsValue::undefined());

        // TODO check whether __wasm_init_memory is called
    }
}

#[deprecated]
pub unsafe fn wasm_mt_apply_patch(mut jump_table: JumpTable) -> Result<(), PatchError> {
    wasm_bindgen_futures::spawn_local(async move {
        use js_sys::{
            ArrayBuffer, Object, Reflect,
            WebAssembly::{self, Memory, Table},
        };
        use wasm_bindgen::prelude::*;
        use wasm_bindgen::JsValue;
        use wasm_bindgen::UnwrapThrowExt;
        use wasm_bindgen_futures::JsFuture;

        let funcs: Table = wasm_bindgen::function_table().unchecked_into();
        let memory: Memory = wasm_bindgen::memory().unchecked_into();
        let exports: Object = wasm_bindgen::exports().unchecked_into();
        let buffer: SharedArrayBuffer = memory.buffer().unchecked_into();

        let module = load_wasm_module(&mut jump_table).await;

        let dylink_section_info = parse_dylink_section(&module).expect("Cannot parse dylink.0 section");

        console_log!("Patch binary data size {}", dylink_section_info.mem_info.memory_size);

        const PAGE_SIZE: u32 = 64 * 1024;
        let page_count = dylink_section_info.mem_info.memory_size.div_ceil(PAGE_SIZE);
        let memory_base = (page_count + 1) * PAGE_SIZE;

        memory.grow(page_count);


        let table_base = funcs.length();

        for v in jump_table.map.values_mut() {
            *v += table_base as u64;
        }

        do_per_thread_hotpatch(table_base, &jump_table, &module.clone(), memory_base).await;

        let web_worker_num = pool_get_web_worker_num();
        let mut hotpatch_state = HOTPATCH_STATE.try_write().expect("cannot lock");
        match *hotpatch_state {
            HotPatchState::Hotpatching(_) => {
                panic!("New hotpatch while hotpatching")
            }
            _ => {}
        }
        *hotpatch_state = HotPatchState::Hotpatching(StateWhenHotpatching {
            jump_table: Some(jump_table),
            remaining_hotpatch_webworker_num: AtomicU32::new(web_worker_num as u32),
        });

        // unlock
        drop(hotpatch_state);

        broadcast_to_workers(
            Arc::new(move |js_value| {
                wasm_bindgen_futures::spawn_local(async move {
                    let module: Module = js_value.into();

                    let hotpatch_state = HOTPATCH_STATE.read().expect("cannot read lock");
                    let is_all_done = match *hotpatch_state {
                        HotPatchState::Hotpatching(ref state_when_hotpatching) => {
                            do_per_thread_hotpatch(
                                table_base,
                                &state_when_hotpatching.jump_table.as_ref().unwrap(),
                                &module,
                                memory_base,
                            )
                            .await;
                            let original = state_when_hotpatching
                                .remaining_hotpatch_webworker_num
                                .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                            original == 1
                        }
                        HotPatchState::HaventHotpatched => panic!("wrong state HaventHotpatched"),
                        HotPatchState::Hotpatched => panic!("wrong state Hotpatched"),
                    };
                    // unlock
                    drop(hotpatch_state);

                    if is_all_done {
                        finalize_hotpatch_after_all_web_workers_loaded_patch();
                    }
                });
            }),
            module.into(),
        )
        .unwrap();
    });

    Ok(())
}

async fn load_wasm_module(jump_table: &JumpTable) -> Module {
    let path = jump_table.lib.to_str().unwrap();

    web_sys::console::info_1(&format!("Going to load wasm binary: {:?}", path).into());

    if !path.ends_with(".wasm") {
        panic!("The binary path in hotpatch message doesn't end with .wasm");
    }

    // Start the fetch of the module
    let response: Promise = web_sys::window().unwrap_throw().fetch_with_str(&path);

    // use compileStreaming instead of compile to enable caching https://v8.dev/blog/wasm-code-caching
    let module_promise = WebAssembly::compile_streaming(&response);

    let module: Module = JsFuture::from(module_promise)
        .await
        .expect("WebAssembly.compileStreaming error")
        .into();

    module
}

pub struct DylinkMemInfo {
    memory_size: u32,
    memory_alignment: u32,
    table_size: u32,
    table_alignment: u32,
}

pub struct DylinkSectionInfo {
    mem_info: DylinkMemInfo,
}

fn read_u8(buf: &mut &[u8]) -> anyhow::Result<u8> {
    let mut local = [0u8];
    buf.read_exact(&mut local)?;
    Ok(local[0])
}

fn parse_dylink_section(module: &Module) -> anyhow::Result<DylinkSectionInfo> {
    let dylink_section_arr = WebAssembly::Module::custom_sections(&module, "dylink.0");
    if dylink_section_arr.length() == 0 {
        panic!("The hotpatch WASM binary doesn't have dylink.0 custom section")
    }
    let dylink_section: ArrayBuffer = dylink_section_arr.get(0).into();
    let dylink_section = Uint8Array::new(&dylink_section);
    let mut dylink_bytes = vec![0u8; dylink_section.length() as usize];
    dylink_section.copy_to(&mut dylink_bytes);

    let mut buf: &[u8] = &dylink_bytes;

    let mut memory_info: Option<DylinkMemInfo> = None;
    loop {
        if buf.len() == 0 {
            break;
        }
        let sub_section_type = read_u8(&mut buf)?;
        let payload_len = leb128::read::unsigned(&mut buf)? as usize;
        let mut sub_buf: &[u8] = &buf[0..payload_len];
        buf = &buf[payload_len..];
        match sub_section_type {
            1 => {
                memory_info = Some(DylinkMemInfo {
                    memory_size: leb128::read::unsigned(&mut sub_buf)? as u32,
                    memory_alignment: leb128::read::unsigned(&mut sub_buf)? as u32,
                    table_size: leb128::read::unsigned(&mut sub_buf)? as u32,
                    table_alignment: leb128::read::unsigned(&mut sub_buf)? as u32,
                });
            }
            _ => {}
        }

        console_log!("Read one subsection in dylink.0")
    }

    Ok(DylinkSectionInfo {
        mem_info: memory_info.context("No memory info")?,
    })
}

pub async fn do_per_thread_hotpatch(
    table_base: u32,
    jump_table: &JumpTable,
    wasm_module: &Module,
    memory_base: u32,
) {
    let funcs: Table = wasm_bindgen::function_table().into();
    let exports: Object = wasm_bindgen::exports().into();

    let old_table_size = funcs.length();

    assert_eq!(old_table_size, table_base);

    // We grow the ifunc table to accommodate the new functions
    // In theory we could just put all the ifuncs in the jump map and use that for our count,
    // but there's no guarantee from the jump table that it references "itself"
    // We might need a sentinel value for each ifunc in the jump map to indicate that it is
    let table_base = funcs
        .grow(jump_table.ifunc_count as u32)
        .expect("growing table");

    // Build up the import object. We copy everything over from the current exports, but then
    // need to add in the memory and table base offsets for the relocations to work.
    //
    // let imports = {
    //     env: {
    //         memory: base.memory,
    //         __tls_base: base.__tls_base,
    //         __stack_pointer: base.__stack_pointer,
    //         __indirect_function_table: base.__indirect_function_table,
    //         __memory_base: memory_base,
    //         __table_base: table_base,
    //        ..base_exports
    //     },
    // };
    let env = Object::new();

    // Move memory, __tls_base, __stack_pointer, __indirect_function_table, and all exports over
    for key in Object::keys(&exports) {
        Reflect::set(
            &env,
            &key,
            &Reflect::get(&exports, &key).expect("getting field from exports"),
        )
        .expect("setting env");
    }

    // Set the memory and table in the imports
    // Following this pattern: Global.new({ value: "i32", mutable: false }, value)
    for (name, value) in [("__table_base", table_base), ("__memory_base", memory_base)] {
        let descriptor = Object::new();
        Reflect::set(&descriptor, &"value".into(), &"i32".into()).expect("setting descriptor");
        Reflect::set(&descriptor, &"mutable".into(), &false.into()).expect("setting descriptor2");
        let value = WebAssembly::Global::new(&descriptor, &value.into()).expect("new global");
        Reflect::set(&env, &name.into(), &value.into()).expect("setting env global");
    }

    // Set the memory and table in the imports
    let imports = Object::new();
    Reflect::set(&imports, &"env".into(), &env).expect("setting env into imports");

    let instance = JsFuture::from(WebAssembly::instantiate_module(wasm_module, &imports))
        .await
        .expect("instantiating module");

    console::log_2(&"result instance".into(), &instance);

    let exports: Object = Reflect::get(&instance, &"exports".into())
        .expect("getting exports")
        .unchecked_into();

    // https://github.com/WebAssembly/tool-conventions/blob/main/DynamicLinking.md#relocations
    _ = Reflect::get(&exports, &"__wasm_apply_data_relocs".into())
        .unwrap()
        .unchecked_into::<js_sys::Function>()
        .call0(&JsValue::undefined());
    _ = Reflect::get(&exports, &"__wasm_apply_global_relocs".into())
        .unwrap()
        .unchecked_into::<js_sys::Function>()
        .call0(&JsValue::undefined());

    // https://github.com/WebAssembly/tool-conventions/blob/main/Linking.md#start-section
    _ = Reflect::get(&exports, &"__wasm_call_ctors".into())
        .unwrap()
        .unchecked_into::<js_sys::Function>()
        .call0(&JsValue::undefined());
}

pub fn finalize_hotpatch_after_all_web_workers_loaded_patch() {
    console_log!("Going to finalize hotpatch");

    // must release read lock before
    let mut hotpatch_state = HOTPATCH_STATE.try_write().expect("cannot lock");

    match *hotpatch_state {
        HotPatchState::HaventHotpatched => panic!("Wrong state HaventHotpatched"),
        HotPatchState::Hotpatching(ref mut s) => {
            assert_eq!(
                s.remaining_hotpatch_webworker_num
                    .load(std::sync::atomic::Ordering::Relaxed),
                0
            );

            let table = mem::replace(&mut s.jump_table, None).expect("jump_table None");

            unsafe { subsecond::commit_patch(table) };
        }
        HotPatchState::Hotpatched => todo!("Wrong state Hotpatched"),
    }

    *hotpatch_state = HotPatchState::Hotpatched;
}

pub enum HotPatchState {
    HaventHotpatched,
    Hotpatching(StateWhenHotpatching),
    Hotpatched,
}

pub struct StateWhenHotpatching {
    jump_table: Option<JumpTable>,
    remaining_hotpatch_webworker_num: AtomicU32,
}

// TODO cannot load new web worker after hotpatching once
// should load max workers upon startup
static HOTPATCH_STATE: RwLock<HotPatchState> = RwLock::new(HotPatchState::HaventHotpatched);
