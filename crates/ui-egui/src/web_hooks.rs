//! Hooks a hosting web page can install (SimpleHub): how a file is asked for and how saved bytes
//! are handed back. Without them the web build uses the browser's file picker and a download.
//! wasm32 is single-threaded, so the hooks live in a thread-local.

use std::cell::RefCell;

use crate::Inbox;

/// What the host does instead of the browser's own file picker and download.
pub struct WebHooks {
    /// Ask the host for one file. The host pushes `(name, bytes)` into the inbox and wakes the
    /// context so the app opens it on the next frame; nothing is pushed when the user cancels.
    pub open: Box<dyn Fn(Inbox, Option<egui::Context>)>,
    /// Hand `bytes` to the host under `name` instead of offering a browser download.
    pub save: Box<dyn Fn(&str, &[u8]) -> Result<(), String>>,
}

thread_local! {
    static HOOKS: RefCell<Option<WebHooks>> = const { RefCell::new(None) };
}

/// Install the hooks; a later call replaces the earlier ones.
pub fn set_web_hooks(hooks: WebHooks) {
    HOOKS.with(|h| *h.borrow_mut() = Some(hooks));
}

/// Whether a host has installed hooks (saves are then kept by the host, not downloaded).
pub(crate) fn installed() -> bool {
    HOOKS.with(|h| h.borrow().is_some())
}

/// Ask the host for a file. Returns `false` when no hooks are installed.
pub(crate) fn open(inbox: Inbox, ctx: Option<egui::Context>) -> bool {
    HOOKS.with(|h| match &*h.borrow() {
        Some(hooks) => {
            (hooks.open)(inbox, ctx);
            true
        }
        None => false,
    })
}

/// Hand bytes to the host. `None` when no hooks are installed.
pub(crate) fn save(name: &str, bytes: &[u8]) -> Option<Result<(), String>> {
    HOOKS.with(|h| h.borrow().as_ref().map(|hooks| (hooks.save)(name, bytes)))
}
