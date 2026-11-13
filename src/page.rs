//! Browser-facing page factory. Native resources remain in their selected adapters.

#[path = "page/api.rs"]
mod api;
pub use api::*;
#[path = "page/selection.rs"]
pub mod selection;

#[cfg(all(target_os = "macos", feature = "engine-cef"))]
#[path = "page/cef.rs"]
pub mod cef;
#[cfg(target_os = "macos")]
#[path = "page/macos.rs"]
mod macos;

use selection::Engine;

pub fn default_engine() -> Engine {
    if cfg!(any(
        feature = "engine-webkit-system",
        feature = "engine-webkit-upstream"
    )) {
        Engine::WebKit
    } else {
        Engine::Cef
    }
}

pub fn available(engine: Engine) -> Result<(), String> {
    let ready = match engine {
        Engine::WebKit => true,
        Engine::Cef => {
            #[cfg(feature = "engine-cef")]
            {
                cef::runtime::available()
            }
            #[cfg(not(feature = "engine-cef"))]
            {
                false
            }
        }
    };
    engine.check_available(
        cfg!(any(
            feature = "engine-webkit-system",
            feature = "engine-webkit-upstream"
        )),
        cfg!(feature = "engine-cef"),
        ready,
    )
}

pub fn info_for(engine: Engine) -> BackendInfo {
    match engine {
        Engine::WebKit => macos::backend_info(),
        Engine::Cef => BackendInfo {
            id: "cef",
            engine_family: "chromium",
            runtime_version: None,
            runtime_identity: RuntimeIdentity::default(),
            capabilities: Capabilities::default(),
        },
    }
}

pub fn backend_info() -> BackendInfo {
    info_for(default_engine())
}

pub fn create_for(
    window: &gpui::Window,
    url: &str,
    engine: Engine,
    hidden: bool,
) -> Result<Page, String> {
    available(engine)?;
    match engine {
        Engine::WebKit => {
            if hidden {
                macos::create_hidden(window, url)
            } else {
                macos::create(window, url)
            }
        }
        Engine::Cef => {
            #[cfg(feature = "engine-cef")]
            {
                cef::runtime::create(window, url, hidden)
            }
            #[cfg(not(feature = "engine-cef"))]
            {
                Err("Chromium (CEF) is not included in this build".into())
            }
        }
    }
}

pub fn create(window: &gpui::Window, url: &str) -> Result<Page, String> {
    create_for(window, url, default_engine(), false)
}

pub fn create_hidden(window: &gpui::Window, url: &str) -> Result<Page, String> {
    create_for(window, url, default_engine(), true)
}
