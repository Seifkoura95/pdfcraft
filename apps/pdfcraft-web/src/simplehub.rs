//! Simple PDF inside SimpleHub. The page mounts the app on a canvas it owns and hands over a
//! bridge object (`open(): Promise<{name, bytes}|null>`, `save(name, bytes): Promise<void>`) that
//! moves documents to and from Simple Cloud; File ▸ Open and every save go through it instead of
//! the browser's file picker and downloads. An optional `theme: "dark" | "light"` on the bridge
//! sets the theme before the first frame, and `PdfHandle::set_theme` switches it while running;
//! both win over the preference saved in the app's own storage.

use std::cell::{Cell, RefCell};

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

    /// Switch the running app to `"dark"` or `"light"` on its next frame. Other values are ignored.
    pub fn set_theme(&self, theme: &str) {
        let Some(theme) = theme_name(theme) else {
            log::warn!("SimpleHub set_theme: unknown theme {theme:?} (expected \"dark\" or \"light\")");
            return;
        };
        PENDING_THEME.with(|t| t.set(Some(theme)));
        // The app may be idle: wake it so the theme is applied now rather than on the next input.
        if let Some(ctx) = APP_CTX.with(|c| c.borrow().clone()) {
            ctx.request_repaint();
        }
    }
}

thread_local! {
    /// A theme the host asked for that the app has not applied yet (wasm32 is single-threaded).
    static PENDING_THEME: Cell<Option<&'static str>> = const { Cell::new(None) };
    /// The running app's context, so `set_theme` can request a repaint.
    static APP_CTX: RefCell<Option<egui::Context>> = const { RefCell::new(None) };
}

/// Normalises a theme name from the host to the app's `theme` option values.
fn theme_name(theme: &str) -> Option<&'static str> {
    match theme.trim().to_ascii_lowercase().as_str() {
        "dark" => Some("dark"),
        "light" => Some("light"),
        _ => None,
    }
}

/// Optional `theme` string on the bridge object.
fn bridge_theme(bridge: &JsValue) -> Option<&'static str> {
    let value = js_sys::Reflect::get(bridge, &JsValue::from_str("theme")).ok()?;
    let name = value.as_string()?;
    let theme = theme_name(&name);
    if theme.is_none() {
        log::warn!("SimpleHub bridge.theme: unknown theme {name:?} (expected \"dark\" or \"light\")");
    }
    theme
}

/// Applies a host theme to the app. `set_option("theme", ..)` sets the app's own preference and
/// pins egui's theme (`ctx.set_theme`) once the context exists, so a system theme change cannot
/// drop it back to egui's stock visuals.
fn apply_theme(app: &mut PdfCraftApp, theme: &str) {
    if let Err(e) = app.set_option("theme", theme) {
        log::error!("SimpleHub theme {theme:?} rejected: {e}");
    }
}

/// Simple PDF with the host's theme requests applied before each frame.
struct HostedApp {
    app: PdfCraftApp,
}

impl eframe::App for HostedApp {
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        self.app.raw_input_hook(ctx, raw);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        self.app.save(storage);
    }

    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        APP_CTX.with(|c| {
            let mut c = c.borrow_mut();
            if c.is_none() {
                *c = Some(ctx.clone());
            }
        });
        if let Some(theme) = PENDING_THEME.with(Cell::take) {
            apply_theme(&mut self.app, theme);
        }
        self.app.logic(ctx, frame);
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.app.ui(ui, frame);
    }
}

/// Starts Simple PDF on `canvas`, with `bridge` providing open and save, and optionally the
/// initial `theme` (`"dark"` or `"light"`).
#[wasm_bindgen]
pub async fn start_simple_pdf(canvas: web_sys::HtmlCanvasElement, bridge: JsValue) -> Result<PdfHandle, JsValue> {
    eframe::WebLogger::init(log::LevelFilter::Info).ok();
    let theme = bridge_theme(&bridge);
    PENDING_THEME.with(|t| t.set(None));
    APP_CTX.with(|c| *c.borrow_mut() = None);
    set_web_hooks(hooks(bridge));
    let options = eframe::WebOptions { renderer: eframe::Renderer::Glow, ..Default::default() };
    let runner = eframe::WebRunner::new();
    runner
        .start(
            canvas,
            options,
            Box::new(move |cc| {
                let mut app = PdfCraftApp::new();
                if let Some(json) = cc.storage.and_then(|s| s.get_string("pdfcraft")) {
                    app.restore(&json);
                }
                // The host's theme wins over the saved preference; applied before the first frame.
                if let Some(theme) = theme {
                    apply_theme(&mut app, theme);
                }
                Ok(Box::new(HostedApp { app }))
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
