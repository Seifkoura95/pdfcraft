//! Simple PDF inside SimpleHub. The page mounts the app on a canvas it owns and hands over a
//! bridge object (`open(): Promise<{name, bytes}|null>`, `save(name, bytes): Promise<void>`) that
//! moves documents to and from Simple Cloud; File ▸ Open and every save go through it instead of
//! the browser's file picker and downloads.

use pdfcraft_ui_egui::{Inbox, PdfCraftApp, WebHooks, set_web_hooks};
use wasm_bindgen::JsCast as _;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

/// A running instance. SimpleHub calls `destroy` when the module unmounts.
#[wasm_bindgen]
pub struct PdfHandle {
    runner: eframe::WebRunner,
}

#[wasm_bindgen]
impl PdfHandle {
    pub fn destroy(&self) {
        self.runner.destroy();
    }

    pub fn has_panicked(&self) -> bool {
        self.runner.has_panicked()
    }
}

/// Starts Simple PDF on `canvas`, with `bridge` providing open and save.
#[wasm_bindgen]
pub async fn start_simple_pdf(canvas: web_sys::HtmlCanvasElement, bridge: JsValue) -> Result<PdfHandle, JsValue> {
    eframe::WebLogger::init(log::LevelFilter::Info).ok();
    set_web_hooks(hooks(bridge));
    let options = eframe::WebOptions { renderer: eframe::Renderer::Glow, ..Default::default() };
    let runner = eframe::WebRunner::new();
    runner
        .start(
            canvas,
            options,
            Box::new(|cc| {
                let mut app = PdfCraftApp::new();
                if let Some(json) = cc.storage.and_then(|s| s.get_string("pdfcraft")) {
                    app.restore(&json);
                }
                Ok(Box::new(app))
            }),
        )
        .await?;
    Ok(PdfHandle { runner })
}

fn hooks(bridge: JsValue) -> WebHooks {
    let open_bridge = bridge.clone();
    WebHooks {
        open: Box::new(move |inbox: Inbox, ctx: Option<egui::Context>| {
            let bridge = open_bridge.clone();
            wasm_bindgen_futures::spawn_local(async move {
                match call_open(&bridge).await {
                    Ok(Some((name, bytes))) => {
                        if let Ok(mut q) = inbox.lock() {
                            q.push((name, bytes));
                        }
                        // The file may arrive while the app is idle: wake it to open the file.
                        if let Some(ctx) = ctx {
                            ctx.request_repaint();
                        }
                    }
                    Ok(None) => {}
                    Err(e) => log::error!("SimpleHub open failed: {e:?}"),
                }
            });
        }),
        save: Box::new(move |name: &str, bytes: &[u8]| {
            let bridge = bridge.clone();
            let name = name.to_string();
            let bytes = js_sys::Uint8Array::from(bytes);
            wasm_bindgen_futures::spawn_local(async move {
                if let Err(e) = call_save(&bridge, &name, &bytes).await {
                    log::error!("SimpleHub save of {name} failed: {e:?}");
                }
            });
            Ok(())
        }),
    }
}

fn method(bridge: &JsValue, name: &str) -> Result<js_sys::Function, JsValue> {
    js_sys::Reflect::get(bridge, &JsValue::from_str(name))?
        .dyn_into::<js_sys::Function>()
        .map_err(|_| JsValue::from_str(&format!("bridge.{name} is not a function")))
}

async fn call_open(bridge: &JsValue) -> Result<Option<(String, Vec<u8>)>, JsValue> {
    let promise: js_sys::Promise = method(bridge, "open")?.call0(bridge)?.dyn_into()?;
    let result = JsFuture::from(promise).await?;
    if result.is_null() || result.is_undefined() {
        return Ok(None);
    }
    let name = js_sys::Reflect::get(&result, &JsValue::from_str("name"))?.as_string().unwrap_or_else(|| "document.pdf".into());
    let bytes: js_sys::Uint8Array = js_sys::Reflect::get(&result, &JsValue::from_str("bytes"))?.dyn_into()?;
    Ok(Some((name, bytes.to_vec())))
}

async fn call_save(bridge: &JsValue, name: &str, bytes: &js_sys::Uint8Array) -> Result<(), JsValue> {
    let ret = method(bridge, "save")?.call2(bridge, &JsValue::from_str(name), bytes)?;
    if let Ok(promise) = ret.dyn_into::<js_sys::Promise>() {
        JsFuture::from(promise).await?;
    }
    Ok(())
}
