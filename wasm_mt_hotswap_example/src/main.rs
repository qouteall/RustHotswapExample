use std::mem;
use std::sync::atomic::{AtomicBool, AtomicU32};
use std::sync::{Arc, RwLock};

use dioxus_devtools::DevserverMsg;
use futures_channel::oneshot;
use js_sys::WebAssembly::Module;
use js_sys::{JsString, Reflect};
use js_sys::{
    Object, Promise, SharedArrayBuffer, Uint8ClampedArray,
    WebAssembly::{self, Memory, Table},
};
use rayon::prelude::*;
use subsecond::{JumpTable, PatchError};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{console, ImageData};
use web_sys::{MessageEvent, WebSocket};

use crate::pool::{pool_get_web_worker_num, submit_to_pool};

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
        console_error_panic_hook::set_once();
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
                rgb_data
                    .par_chunks_mut(4)
                    .enumerate()
                    .for_each(|(i, chunk)| {
                        let i = i as u32;
                        let x = i % width;
                        let y = i / width;
                        let ray = raytracer::Ray::create_prime(x, y, &scene);
                        let result = raytracer::cast_ray(&scene, &ray, 0).to_rgba();
                        chunk[0] = result.data[0];
                        chunk[1] = result.data[1];
                        chunk[2] = result.data[2];
                        chunk[3] = result.data[3];
                    });
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
    init_hotpatch(Box::new(|| {
        console::log_1(&"Hotpatched".into());
    }));

    console::log_1(&"Hello world from Rust WASM!".into());
}

#[cfg(not(debug_assertions))]
fn init_hotpatch(on_hotpatch_callback: Box<dyn Fn()>) {
    // empty in release
}

// https://github.com/DioxusLabs/dioxus/blob/main/packages/web/src/devtools.rs
#[cfg(debug_assertions)]
fn init_hotpatch(on_hotpatch_callback: Box<dyn Fn()>) {
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
                        unsafe {
                            wasm_mt_apply_patch(jumptable);
                        }

                        on_hotpatch_callback();
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

        let path = jump_table.lib.to_str().unwrap();

        web_sys::console::info_1(&format!("Going to load wasm binary: {:?}", path).into());

        if !path.ends_with(".wasm") {
            web_sys::console::error_1(&"doesn't end with .wasm, ignore".into());
            return;
        }

        // Start the fetch of the module
        let response: Promise = web_sys::window().unwrap_throw().fetch_with_str(&path);

        // Wait for the fetch to complete - we need the wasm module size in bytes to reserve in the memory
        let response: web_sys::Response = JsFuture::from(response).await.unwrap().unchecked_into();

        // If the status is not success, we bail
        if !response.ok() {
            panic!(
                "Failed to patch wasm module at {} - response failed with: {}",
                path,
                response.status_text()
            );
        }

        let dl_bytes: ArrayBuffer = JsFuture::from(response.array_buffer().unwrap())
            .await
            .unwrap()
            .unchecked_into();

        // Expand the memory and table size to accommodate the new data and functions
        //
        // Normally we wouldn't be able to trust that we are allocating *enough* memory
        // for BSS segments, but ld emits them in the binary when using import-memory.
        //
        // Make sure we align the memory base to the page size
        // TODO it seems grows too much. only need to grow as size of data section
        const PAGE_SIZE: u32 = 64 * 1024;
        let page_count = (buffer.byte_length() as f64 / PAGE_SIZE as f64).ceil() as u32;
        let memory_base = (page_count + 1) * PAGE_SIZE;

        memory.grow((dl_bytes.byte_length() as f64 / PAGE_SIZE as f64).ceil() as u32 + 1);

        let module_promise = WebAssembly::compile_streaming(dl_bytes.unchecked_ref());
        let module = JsFuture::from(module_promise).await.unwrap();

        let table_base = funcs.length();

        for v in jump_table.map.values_mut() {
            *v += table_base as u64;
        }

        do_per_thread_hotpatch(table_base, &jump_table, &module.unchecked_into::<Module>(), memory_base);

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

        todo!()
    });

    Ok(())
}

pub async fn do_per_thread_hotpatch(
    table_base: u32,
    jump_table: &JumpTable,
    wasm_module: &Module,
    memory_base: u32,
) {
    let funcs: Table = wasm_bindgen::function_table().unchecked_into();
    let memory: Memory = wasm_bindgen::memory().unchecked_into();
    let exports: Object = wasm_bindgen::exports().unchecked_into();
    let buffer: SharedArrayBuffer = memory.buffer().unchecked_into();

    let old_table_size = funcs.length();

    assert_eq!(old_table_size, table_base);

    // We grow the ifunc table to accommodate the new functions
    // In theory we could just put all the ifuncs in the jump map and use that for our count,
    // but there's no guarantee from the jump table that it references "itself"
    // We might need a sentinel value for each ifunc in the jump map to indicate that it is
    let table_base = funcs.grow(jump_table.ifunc_count as u32).unwrap();

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
    // TODO see if it changes in multithreading
    let env = Object::new();

    // Move memory, __tls_base, __stack_pointer, __indirect_function_table, and all exports over
    for key in Object::keys(&exports) {
        Reflect::set(&env, &key, &Reflect::get(&exports, &key).unwrap()).unwrap();
    }

    // Set the memory and table in the imports
    // Following this pattern: Global.new({ value: "i32", mutable: false }, value)
    for (name, value) in [("__table_base", table_base), ("__memory_base", memory_base)] {
        let descriptor = Object::new();
        Reflect::set(&descriptor, &"value".into(), &"i32".into()).unwrap();
        Reflect::set(&descriptor, &"mutable".into(), &false.into()).unwrap();
        let value = WebAssembly::Global::new(&descriptor, &value.into()).unwrap();
        Reflect::set(&env, &name.into(), &value.into()).unwrap();
    }

    // Set the memory and table in the imports
    let imports = Object::new();
    Reflect::set(&imports, &"env".into(), &env).unwrap();

    let result_object = JsFuture::from(WebAssembly::instantiate_module(wasm_module, &imports))
        .await
        .unwrap();

    // We need to run the data relocations and then fire off the constructors
    let res: Object = result_object.unchecked_into();
    let instance: Object = Reflect::get(&res, &"instance".into())
        .unwrap()
        .unchecked_into();
    let exports: Object = Reflect::get(&instance, &"exports".into())
        .unwrap()
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
