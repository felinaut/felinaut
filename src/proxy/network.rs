//! Route every page through a SOCKS5 proxy with `WKWebsiteDataStore.proxyConfigurations`.
//!
//! All Felinaut pages share WebKit's default data store, so one setting covers every tab, the
//! hidden live-folder page and downloads. The Network framework's proxy functions exist from
//! macOS 14, while Felinaut runs on macOS 12, so they are looked up at run time instead of
//! linked: linking them would stop older systems from launching the app at all.

use std::{
    ffi::{CStr, CString, c_char, c_int, c_void},
    sync::OnceLock,
};

use objc2::{
    class, msg_send,
    rc::Retained,
    runtime::{AnyObject, Bool},
    sel,
};
use objc2_foundation::NSArray;

use super::model::Proxy;

type Object = *mut AnyObject;

unsafe extern "C" {
    fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}
const RTLD_LAZY: c_int = 0x1;
const RTLD_LOCAL: c_int = 0x4;

type CreateHost = unsafe extern "C" fn(*const c_char, *const c_char) -> Object;
type CreateSocks5 = unsafe extern "C" fn(Object) -> Object;
type SetCredentials = unsafe extern "C" fn(Object, *const c_char, *const c_char);
type SetFailover = unsafe extern "C" fn(Object, bool);

/// The Network framework functions used here, all available from macOS 14.
struct Api {
    create_host: CreateHost,
    create_socksv5: CreateSocks5,
    set_credentials: SetCredentials,
    set_failover: SetFailover,
}

fn api() -> Option<&'static Api> {
    static API: OnceLock<Option<Api>> = OnceLock::new();
    API.get_or_init(|| {
        const NETWORK: &CStr = c"/System/Library/Frameworks/Network.framework/Network";
        // SAFETY: dlopen and dlsym only read the NUL-terminated strings. The framework stays
        // loaded for the life of the process (WebKit links it too), so the function pointers
        // never dangle. Each symbol is cast to the signature in Apple's <Network/*.h> headers.
        unsafe {
            let handle = dlopen(NETWORK.as_ptr(), RTLD_LAZY | RTLD_LOCAL);
            if handle.is_null() {
                return None;
            }
            let symbol = |name: &CStr| {
                let pointer = dlsym(handle, name.as_ptr());
                (!pointer.is_null()).then_some(pointer)
            };
            Some(Api {
                create_host: std::mem::transmute::<*mut c_void, CreateHost>(symbol(
                    c"nw_endpoint_create_host",
                )?),
                create_socksv5: std::mem::transmute::<*mut c_void, CreateSocks5>(symbol(
                    c"nw_proxy_config_create_socksv5",
                )?),
                set_credentials: std::mem::transmute::<*mut c_void, SetCredentials>(symbol(
                    c"nw_proxy_config_set_username_and_password",
                )?),
                set_failover: std::mem::transmute::<*mut c_void, SetFailover>(symbol(
                    c"nw_proxy_config_set_failover_allowed",
                )?),
            })
        }
    })
    .as_ref()
}

fn data_store() -> Retained<AnyObject> {
    // SAFETY: WebKit is linked (through Wry), so the class exists; `defaultDataStore` returns
    // a shared, autoreleased store that we retain.
    unsafe { msg_send![class!(WKWebsiteDataStore), defaultDataStore] }
}

/// Whether this macOS can send pages through a proxy (macOS 14 and later).
pub fn supported() -> bool {
    if api().is_none() {
        return false;
    }
    let store = data_store();
    // SAFETY: `respondsToSelector:` is safe to send to any NSObject.
    let responds: Bool =
        unsafe { msg_send![&store, respondsToSelector: sel!(setProxyConfigurations:)] };
    responds.as_bool()
}

#[derive(Debug, PartialEq, Eq)]
pub enum ApplyError {
    Unsupported,
    InvalidProxy,
}

/// Send all pages through `proxy`, signing in with `password` when it has a user name, or
/// connect directly when `proxy` is `None`. Must run on the main thread. Connections fail
/// rather than fall back to a direct connection when the proxy cannot be used.
pub fn apply(proxy: Option<(&Proxy, Option<&str>)>) -> Result<(), ApplyError> {
    let Some(api) = api() else {
        return Err(ApplyError::Unsupported);
    };
    if !supported() {
        return Err(ApplyError::Unsupported);
    }
    let configurations: Retained<NSArray<AnyObject>> = match proxy {
        None => NSArray::new(),
        Some((proxy, password)) => {
            NSArray::from_retained_slice(&[configuration(api, proxy, password)?])
        }
    };
    let store = data_store();
    // SAFETY: checked above that the store responds to the selector; it takes an NSArray of
    // nw_proxy_config_t objects (macOS 14 `-[WKWebsiteDataStore setProxyConfigurations:]`).
    unsafe {
        let _: () = msg_send![&store, setProxyConfigurations: &*configurations];
    }
    Ok(())
}

fn configuration(
    api: &Api,
    proxy: &Proxy,
    password: Option<&str>,
) -> Result<Retained<AnyObject>, ApplyError> {
    let c = |text: &str| CString::new(text).map_err(|_| ApplyError::InvalidProxy);
    let host = c(&proxy.host)?;
    let port = c(&proxy.port.to_string())?;
    let login = match &proxy.username {
        Some(user) => Some((c(user)?, c(password.unwrap_or(""))?)),
        None => None,
    };
    // SAFETY: the strings outlive the calls, which copy them. Network objects are Objective-C
    // objects returned retained (+1) under ARC conventions, so `Retained::from_raw` takes over
    // that reference; null means failure. The configuration retains its endpoint.
    unsafe {
        let endpoint = Retained::from_raw((api.create_host)(host.as_ptr(), port.as_ptr()))
            .ok_or(ApplyError::InvalidProxy)?;
        let config =
            Retained::from_raw((api.create_socksv5)(Retained::as_ptr(&endpoint).cast_mut()))
                .ok_or(ApplyError::InvalidProxy)?;
        let raw = Retained::as_ptr(&config).cast_mut();
        if let Some((user, pass)) = &login {
            (api.set_credentials)(raw, user.as_ptr(), pass.as_ptr());
        }
        // Never quietly send a page around a proxy the user chose.
        (api.set_failover)(raw, false);
        Ok(config)
    }
}
