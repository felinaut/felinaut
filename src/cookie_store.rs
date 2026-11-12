//! WebKit's cookie store for the Cookies window: list every cookie, delete some.
//!
//! Wry gives each webview `WKWebsiteDataStore.defaultDataStore` (Felinaut asks for neither
//! incognito nor a data store identifier), so that store's `WKHTTPCookieStore` holds exactly
//! the cookies the pages and the hidden live-folder webview use. It is persistent: WebKit
//! writes cookies that have an expiry to disk itself and drops session cookies at quit.
//!
//! WebKit answers on the main thread, some time later. The answers only go into a channel
//! that the window drains; nothing here calls into GPUI, and nothing here logs a cookie.

use std::cell::Cell;
use std::collections::HashSet;
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::mpsc::Sender;

use block2::RcBlock;
use objc2::{MainThreadMarker, rc::Retained};
use objc2_foundation::{NSArray, NSHTTPCookie};
use objc2_web_kit::{WKHTTPCookieStore, WKWebsiteDataStore};

use crate::cookies::{Cookie, Key, SameSite};

fn store() -> Result<Retained<WKHTTPCookieStore>, String> {
    let mtm = MainThreadMarker::new().ok_or("Cookies can only be read on the main thread")?;
    // SAFETY: WebKit's shared default store and its cookie store, read on the main thread.
    Ok(unsafe { WKWebsiteDataStore::defaultDataStore(mtm).httpCookieStore() })
}

fn read(cookie: &NSHTTPCookie) -> Cookie {
    let same_site = match cookie.sameSitePolicy() {
        Some(policy) if policy.to_string().eq_ignore_ascii_case("strict") => SameSite::Strict,
        Some(policy) if policy.to_string().eq_ignore_ascii_case("lax") => SameSite::Lax,
        _ => SameSite::None,
    };
    let expires = cookie
        .expiresDate()
        .filter(|_| !cookie.isSessionOnly())
        // Saturating: a date far outside the calendar stays a number the model can show.
        .map(|date| date.timeIntervalSince1970() as i64);
    Cookie {
        name: cookie.name().to_string(),
        value: cookie.value().to_string(),
        domain: cookie.domain().to_string(),
        path: cookie.path().to_string(),
        expires,
        secure: cookie.isSecure(),
        http_only: cookie.isHTTPOnly(),
        same_site,
    }
}

fn key(cookie: &NSHTTPCookie) -> Key {
    Key {
        domain: cookie.domain().to_string(),
        path: cookie.path().to_string(),
        name: cookie.name().to_string(),
    }
}

/// Asks WebKit for every stored cookie. The list arrives on `reply` later.
pub fn fetch(reply: Sender<Vec<Cookie>>) -> Result<(), String> {
    let store = store()?;
    let block = RcBlock::new(move |cookies: NonNull<NSArray<NSHTTPCookie>>| {
        // SAFETY: WebKit passes a live array for the duration of the call.
        let cookies = unsafe { cookies.as_ref() }.to_vec();
        let _ = reply.send(cookies.iter().map(|cookie| read(cookie)).collect());
    });
    // SAFETY: a live cookie store on the main thread; WebKit copies the block.
    unsafe { store.getAllCookies(&block) };
    Ok(())
}

/// Deletes the cookies with these keys. When WebKit has confirmed every deletion, the
/// cookies that remain arrive on `reply`, as for `fetch`.
pub fn delete(keys: HashSet<Key>, reply: Sender<Vec<Cookie>>) -> Result<(), String> {
    let store = store()?;
    let cookie_store = store.clone();
    // WebKit deletes by cookie object, so look the keys up in what is stored now.
    let block = RcBlock::new(move |cookies: NonNull<NSArray<NSHTTPCookie>>| {
        // SAFETY: WebKit passes a live array for the duration of the call.
        let mut doomed = unsafe { cookies.as_ref() }.to_vec();
        doomed.retain(|cookie| keys.contains(&key(cookie)));
        if doomed.is_empty() {
            let _ = fetch(reply.clone());
            return;
        }
        let left = Rc::new(Cell::new(doomed.len()));
        for cookie in &doomed {
            let (left, reply) = (left.clone(), reply.clone());
            let done = RcBlock::new(move || {
                left.set(left.get().saturating_sub(1));
                if left.get() == 0 {
                    let _ = fetch(reply.clone());
                }
            });
            // SAFETY: the same live store, on the main thread (WebKit calls this block there),
            // with a cookie it just handed out; it copies the completion block.
            unsafe { cookie_store.deleteCookie_completionHandler(cookie, Some(&done)) };
        }
    });
    // SAFETY: a live cookie store on the main thread; WebKit copies the block.
    unsafe { store.getAllCookies(&block) };
    Ok(())
}
