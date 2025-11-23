use std::cell::RefCell;
use std::rc::Rc;

use dioxus_devtools::subsecond::apply_patch;
use dioxus_devtools::{DevserverMsg};
use wasm_bindgen::prelude::*;
use web_sys::js_sys::JsString;
use web_sys::{console, MessageEvent, WebSocket};

// const FAVICON: Asset = asset!("/assets/favicon.ico");
// const MAIN_CSS: Asset = asset!("/assets/main.css");
// const HEADER_SVG: Asset = asset!("/assets/header.svg");

fn main() {
    // this is just placeholder
    // it won't be called when a #[wasm_bindgen(start)] function exists, because
    // https://github.com/DioxusLabs/dioxus/blob/f7e102a0b4868f51f35059ddacb19d78f10f0fa6/packages/cli/src/build/request.rs#L4242
}

#[wasm_bindgen(start)]
pub fn start() {
    init_hotpatch(Box::new(|| {
        console::log_1(&"Hotpatched".into());
    }));

    console::log_1(&"Hello world from Rust WASM!".into());

    init_counter();
}

fn init_counter() {
    let window = web_sys::window().expect("no global `window` exists");
    let document = window.document().expect("should have a document on window");

    let button = document
        .get_element_by_id("incrementButton")
        .expect("missing element #incrementButton");

    let counter_display = document
        .get_element_by_id("counter")
        .expect("missing element #counter");

    let count = Rc::new(RefCell::new(0));

    let count_clone = count.clone();
    let display_clone = counter_display.clone();

    let closure = Closure::<dyn FnMut()>::new(move || {
        subsecond::call(|| {
            // *count_clone.borrow_mut() += 1;
            *count_clone.borrow_mut() *= 2;

            display_clone.set_text_content(Some(&count_clone.borrow().to_string()));
        })
    });

    button
        .add_event_listener_with_callback("click", closure.as_ref().unchecked_ref())
        .expect("failed to add event listener");

    closure.forget(); // leaking is fine as it's global singleton

    console::log_1(&"Counter initialized".into());
}

#[cfg(not(debug_assertions))]
fn init_hotpatch(on_hotpatch_callback: Box<dyn Fn()>) {
    // empty in release
}

// https://github.com/DioxusLabs/dioxus/blob/main/packages/web/src/devtools.rs
#[cfg(debug_assertions)]
fn init_hotpatch(on_hotpatch_callback: Box<dyn Fn()>) {
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
                        unsafe { apply_patch(jumptable).unwrap() };
                    }

                    on_hotpatch_callback();
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
