//! The WebKit side of JavaScript dialogs.
//!
//! Wry's `WKUIDelegate` class has no dialog methods, so WebKit answers each dialog itself
//! without showing it. This module adds the three methods. Everything here runs on the main
//! thread. A WebKit callback only puts the dialog in its page's queue; the browser's poll
//! finds it there. A callback never calls into GPUI.

use std::{cell::RefCell, collections::HashMap};

use block2::Block;
use objc2::{
    msg_send,
    rc::Retained,
    runtime::{AnyObject, Bool, Imp, Sel},
    sel,
};
use objc2_foundation::NSString;
use objc2_web_kit::WKWebView;

use super::{
    Kind, MESSAGE_LIMIT, Queue, Reply, Request, Responder, Shown, TEXT_LIMIT, origin, url_origin,
};

thread_local! {
    /// The queue of each page that may show dialogs, by its WKWebView's address.
    static PAGES: RefCell<HashMap<usize, Queue<Responder>>> = RefCell::default();
}

/// Routes the dialogs of `view` here. Only a page the user can see (`allowed`) may show one;
/// each dialog of another page gets the default answer at once. Call `detach` when an
/// allowed page goes away.
pub fn attach(view: &WKWebView, allowed: bool) {
    type Alert =
        unsafe extern "C-unwind" fn(Receiver, Sel, Object, Text, Object, *mut Block<dyn Fn()>);
    unsafe extern "C-unwind" fn alert(
        _: Receiver,
        _: Sel,
        webview: Object,
        message: Text,
        frame: Object,
        handler: *mut Block<dyn Fn()>,
    ) {
        // SAFETY: WebKit passes a live completion handler. `copy` moves it to the heap and
        // keeps it alive after this method returns.
        let handler = unsafe { &*handler }.copy();
        let request = request(Kind::Alert, message, std::ptr::null_mut(), frame);
        add(webview, request, Responder::new(move |_| handler.call(())));
    }
    type Confirm =
        unsafe extern "C-unwind" fn(Receiver, Sel, Object, Text, Object, *mut Block<dyn Fn(Bool)>);
    unsafe extern "C-unwind" fn confirm(
        _: Receiver,
        _: Sel,
        webview: Object,
        message: Text,
        frame: Object,
        handler: *mut Block<dyn Fn(Bool)>,
    ) {
        // SAFETY: as in `alert`.
        let handler = unsafe { &*handler }.copy();
        let request = request(Kind::Confirm, message, std::ptr::null_mut(), frame);
        let responder =
            Responder::new(move |reply: Reply| handler.call((Bool::new(reply.accepted),)));
        add(webview, request, responder);
    }
    type Prompt = unsafe extern "C-unwind" fn(
        Receiver,
        Sel,
        Object,
        Text,
        Text,
        Object,
        *mut Block<dyn Fn(*mut NSString)>,
    );
    unsafe extern "C-unwind" fn prompt(
        _: Receiver,
        _: Sel,
        webview: Object,
        message: Text,
        default_text: Text,
        frame: Object,
        handler: *mut Block<dyn Fn(*mut NSString)>,
    ) {
        // SAFETY: as in `alert`.
        let handler = unsafe { &*handler }.copy();
        let request = request(Kind::Prompt, message, default_text, frame);
        let responder = Responder::new(move |reply: Reply| {
            // A null string is JavaScript's `null`. WebKit copies the string during the call.
            let text = reply.prompt_value().map(|text| NSString::from_str(&text));
            let pointer = text.as_ref().map_or(std::ptr::null_mut(), |text| {
                Retained::as_ptr(text).cast_mut()
            });
            handler.call((pointer,));
        });
        add(webview, request, responder);
    }

    // SAFETY: a property read on a live WKWebView, on the main thread. Wry keeps the delegate
    // alive (WKWebView only holds it weakly) for as long as the WebView.
    let delegate: Option<Retained<AnyObject>> = unsafe { msg_send![view, UIDelegate] };
    let Some(delegate) = delegate else {
        return;
    };
    // SAFETY: `class` is Wry's registered UI delegate class. Adding a method only succeeds if
    // the class lacks it, so Wry's own methods are never replaced and later pages add nothing.
    // Each function matches its type encoding: void, self, _cmd, then the objects (`@`) of
    // its selector, the last one a block (`@?`).
    unsafe {
        let class = delegate.class() as *const _ as *mut _;
        objc2::ffi::class_addMethod(
            class,
            sel!(webView:runJavaScriptAlertPanelWithMessage:initiatedByFrame:completionHandler:),
            std::mem::transmute::<Alert, Imp>(alert),
            c"v@:@@@@?".as_ptr(),
        );
        objc2::ffi::class_addMethod(
            class,
            sel!(webView:runJavaScriptConfirmPanelWithMessage:initiatedByFrame:completionHandler:),
            std::mem::transmute::<Confirm, Imp>(confirm),
            c"v@:@@@@?".as_ptr(),
        );
        objc2::ffi::class_addMethod(
            class,
            sel!(webView:runJavaScriptTextInputPanelWithPrompt:defaultText:initiatedByFrame:completionHandler:),
            std::mem::transmute::<Prompt, Imp>(prompt),
            c"v@:@@@@@?".as_ptr(),
        );
    }
    // WebKit caches which methods a delegate implements when it is set, so set it again.
    // SAFETY: the same live delegate, on the main thread.
    let _: () = unsafe { msg_send![view, setUIDelegate: &*delegate] };
    if allowed {
        let key = view as *const WKWebView as usize;
        PAGES.with_borrow_mut(|pages| pages.insert(key, Queue::default()));
    }
}

type Receiver = *mut AnyObject;
type Object = *mut AnyObject;
type Text = *mut NSString;

/// Reads at most `limit` characters of page text. The string of the page can be very long,
/// so it is cut before the copy to Rust.
fn text(text: Text, limit: usize) -> String {
    // SAFETY: WebKit passes a live string or null, on the main thread.
    let Some(text) = (unsafe { text.as_ref() }) else {
        return String::new();
    };
    // One character is at most two UTF-16 units. The extra unit lets the model see that the
    // text was longer and mark the cut.
    let units = 2 * limit + 1;
    if text.length() <= units {
        return text.to_string();
    }
    // Do not cut a character in two.
    let end = text.rangeOfComposedCharacterSequenceAtIndex(units).location;
    text.substringToIndex(end).to_string()
}

fn request(kind: Kind, message: Text, default_text: Text, frame: Object) -> Request {
    // SAFETY: WebKit passes a live WKFrameInfo, or null, on the main thread. These are its
    // public properties, with the types WebKit declares.
    let (origin, main_frame): (String, bool) = match unsafe { frame.as_ref() } {
        Some(frame) => unsafe {
            let security: Retained<AnyObject> = msg_send![frame, securityOrigin];
            let scheme: Retained<NSString> = msg_send![&*security, protocol];
            let host: Retained<NSString> = msg_send![&*security, host];
            let port: isize = msg_send![&*security, port];
            // No host name is longer than 253 characters.
            let origin = origin(
                &text(Retained::as_ptr(&scheme).cast_mut(), 16),
                &text(Retained::as_ptr(&host).cast_mut(), 256),
                u16::try_from(port).unwrap_or(0),
            );
            (origin, msg_send![frame, isMainFrame])
        },
        None => (String::new(), true),
    };
    Request::new(
        kind,
        origin,
        main_frame,
        &text(message, MESSAGE_LIMIT),
        &text(default_text, TEXT_LIMIT),
    )
}

/// Runs inside a WebKit callback. A page without a queue (a live folder's reader), a full
/// queue and an origin whose dialogs the user stopped get the default answer at once.
fn add(webview: Object, request: Request, responder: Responder) {
    let refused = PAGES.with_borrow_mut(|pages| match pages.get_mut(&(webview as usize)) {
        Some(queue) => queue.push(request, responder).err(),
        None => Some(responder),
    });
    // The borrow has ended: dropping the responder calls WebKit's handler.
    drop(refused);
}

/// Forgets a page given to `attach`, by its WKWebView's address. Its dialogs get the default
/// answer.
pub fn detach(page: usize) {
    let queue = PAGES.with_borrow_mut(|pages| pages.remove(&page));
    drop(queue);
}

/// The dialog `page` shows now.
pub fn shown(page: usize) -> Option<Shown> {
    PAGES.with_borrow(|pages| pages.get(&page)?.shown())
}

/// Gives the user's answer to dialog `id` of `page`. With `suppress`, the origin of that
/// dialog can show no more dialogs in this page. Does nothing if that dialog is no longer on show.
pub fn answer(page: usize, id: u64, reply: Reply, suppress: bool) {
    let answered = PAGES.with_borrow_mut(|pages| pages.get_mut(&page)?.answer(id, suppress));
    if let Some((responder, rest)) = answered {
        responder.send(reply);
        drop(rest);
    }
}

/// Gives the default answer to all dialogs of `page`: nobody can see the page.
pub fn dismiss(page: usize) {
    let dismissed = PAGES.with_borrow_mut(|pages| pages.get_mut(&page).map(Queue::dismiss));
    drop(dismissed);
}

/// The user starts a navigation in `page`. Its dialogs get the default answer.
pub fn user_navigates(page: usize) {
    let dismissed = PAGES.with_borrow_mut(|pages| pages.get_mut(&page).map(Queue::user_navigates));
    drop(dismissed);
}

/// `page` committed the document at `url`: the dialogs of the old one get the default answer.
/// Runs inside a WebKit callback.
pub fn commit(page: usize, url: &str) {
    let origin = url_origin(url);
    let dismissed = PAGES.with_borrow_mut(|pages| Some(pages.get_mut(&page)?.commit(&origin)));
    drop(dismissed);
}
