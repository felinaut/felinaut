//! The WebKit side of site permissions.
//!
//! Wry answers a camera or microphone request inside the delegate callback, and it grants
//! each request when no handler is set. Its handler cannot wait for the user and does not see
//! the origin. Felinaut therefore replaces the
//! `webView:requestMediaCapturePermissionForOrigin:initiatedByFrame:type:decisionHandler:`
//! method of Wry's UI delegate. The replacement denies at once or keeps the request until the
//! browser answers it. Everything here runs on the main thread, inside a WebKit callback or
//! the browser's poll, and never calls into GPUI.

use std::cell::RefCell;

use objc2::{
    msg_send,
    rc::Retained,
    runtime::{AnyObject, Imp, Sel},
    sel,
};
use objc2_foundation::NSString;
use objc2_web_kit::WKWebView;

use super::{Kind, askable, kinds, origin};

/// `WKPermissionDecisionGrant` and `WKPermissionDecisionDeny`. Felinaut never answers
/// `WKPermissionDecisionPrompt` (0), which would show WebKit's own prompt.
const GRANT: isize = 1;
const DENY: isize = 2;
/// A page gets no prompt while this many requests wait: more are denied.
const MAX_PENDING: usize = 16;

type Handler = block2::Block<dyn Fn(isize)>;

/// WebKit's decision handler for one request. WebKit raises an exception when the block goes
/// away without a call, and it ignores a second call. `send` and `Drop` both take the block
/// out, so each request gets exactly one answer, and an answer that nobody gave is a denial.
struct Reply(Option<block2::RcBlock<dyn Fn(isize)>>);

impl Reply {
    fn send(mut self, grant: bool) {
        if let Some(handler) = self.0.take() {
            handler.call((if grant { GRANT } else { DENY },));
        }
    }
}

impl Drop for Reply {
    fn drop(&mut self) {
        if let Some(handler) = self.0.take() {
            handler.call((DENY,));
        }
    }
}

/// A request that waits for the browser's answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub id: u64,
    /// The page that asks, by its WKWebView's address.
    pub page: usize,
    pub origin: String,
    pub kinds: &'static [Kind],
}

#[derive(Default)]
struct Native {
    /// The pages that may ask, by their WKWebView's address.
    pages: Vec<usize>,
    pending: Vec<(Request, Reply)>,
    last_id: u64,
}

thread_local! {
    static NATIVE: RefCell<Native> = RefCell::default();
}

/// Sends camera and microphone requests of `view` here. Only a page the user can see
/// (`visible`) may ask; each request of any other page is denied. Call `detach` when a page
/// goes away.
pub fn attach(view: &WKWebView, visible: bool) {
    type Callback = unsafe extern "C-unwind" fn(
        *mut AnyObject,
        Sel,
        *mut AnyObject,
        *mut AnyObject,
        *mut AnyObject,
        isize,
        *mut Handler,
    );
    unsafe extern "C-unwind" fn requested(
        _: *mut AnyObject,
        _: Sel,
        webview: *mut AnyObject,
        top: *mut AnyObject,
        frame: *mut AnyObject,
        capture_type: isize,
        handler: *mut Handler,
    ) {
        // SAFETY: WebKit passes a live block on the main thread. `copy` retains it, so the
        // `Reply` can outlive this callback.
        let Some(handler) = (unsafe { handler.as_ref() }) else {
            return;
        };
        let reply = Reply(Some(handler.copy()));
        // SAFETY: WebKit passes a live WKFrameInfo, or null.
        let frame = unsafe { frame.as_ref() }.map(|frame| -> Retained<AnyObject> {
            // SAFETY: `securityOrigin` is a non-null property of WKFrameInfo.
            unsafe { msg_send![frame, securityOrigin] }
        });
        request(
            webview as usize,
            read_origin(top),
            read_origin(frame.as_ref().map_or(std::ptr::null(), Retained::as_ptr)),
            capture_type,
            reply,
        );
    }

    // SAFETY: a property read on a live WKWebView, on the main thread. Wry keeps the delegate
    // alive (WKWebView only holds it weakly) for as long as the WebView.
    let delegate: Option<Retained<AnyObject>> = unsafe { msg_send![view, UIDelegate] };
    let Some(delegate) = delegate else {
        // Wry always sets one. Without it WebKit takes its default action, not Wry's grant.
        return;
    };
    // SAFETY: `class` is Wry's UI delegate class. The replacement has the type encoding of the
    // method it replaces: void, self, _cmd, three objects, an NSInteger and a block. Replacing
    // again for a later page changes nothing.
    unsafe {
        objc2::ffi::class_replaceMethod(
            delegate.class() as *const _ as *mut _,
            sel!(webView:requestMediaCapturePermissionForOrigin:initiatedByFrame:type:decisionHandler:),
            std::mem::transmute::<Callback, Imp>(requested),
            c"v@:@@@q@?".as_ptr(),
        );
    }
    // WebKit caches which methods a delegate implements when it is set, so set it again.
    // SAFETY: the same live delegate, on the main thread.
    let _: () = unsafe { msg_send![view, setUIDelegate: &*delegate] };
    if visible {
        let key = view as *const WKWebView as usize;
        NATIVE.with_borrow_mut(|native| native.pages.push(key));
    }
}

/// The `origin` key of a WKSecurityOrigin, if that origin may ask.
fn read_origin(security_origin: *const AnyObject) -> Option<String> {
    // SAFETY: a live WKSecurityOrigin, or null, on the main thread.
    let security_origin = unsafe { security_origin.as_ref() }?;
    // SAFETY: property reads with the types WebKit declares.
    let (scheme, host, port): (Retained<NSString>, Retained<NSString>, isize) = unsafe {
        (
            msg_send![security_origin, protocol],
            msg_send![security_origin, host],
            msg_send![security_origin, port],
        )
    };
    origin(&scheme.to_string(), &host.to_string(), port as i64)
}

/// Keeps the request for the browser, or drops `reply`, which denies it.
fn request(
    page: usize,
    top: Option<String>,
    frame: Option<String>,
    capture_type: isize,
    reply: Reply,
) {
    let denied = NATIVE.with_borrow_mut(|native| {
        let waiting = native
            .pending
            .iter()
            .filter(|(request, _)| request.page == page)
            .count();
        let asked = askable(native.pages.contains(&page), top, frame)
            .zip(kinds(capture_type))
            .filter(|_| waiting < MAX_PENDING);
        let Some((origin, kinds)) = asked else {
            return Some(reply);
        };
        native.last_id += 1;
        let request = Request {
            id: native.last_id,
            page,
            origin,
            kinds,
        };
        native.pending.push((request, reply));
        None
    });
    // Outside the borrow: the denial calls into WebKit.
    drop(denied);
}

/// The requests that wait for an answer, oldest first. The browser polls this on the UI
/// thread.
pub fn requests() -> Vec<Request> {
    NATIVE.with_borrow(|native| {
        native
            .pending
            .iter()
            .map(|(request, _)| request.clone())
            .collect()
    })
}

/// Answers a waiting request. An unknown id, such as that of a closed page, does nothing.
pub fn answer(id: u64, grant: bool) {
    let reply = NATIVE.with_borrow_mut(|native| {
        let index = native
            .pending
            .iter()
            .position(|(request, _)| request.id == id)?;
        Some(native.pending.remove(index).1)
    });
    if let Some(reply) = reply {
        reply.send(grant);
    }
}

/// Forgets a page given to `attach` and denies its waiting requests. Call it before the
/// WKWebView goes away.
pub fn detach(page: usize) {
    let denied: Vec<_> = NATIVE.with_borrow_mut(|native| {
        native.pages.retain(|known| *known != page);
        let (denied, kept) = std::mem::take(&mut native.pending)
            .into_iter()
            .partition(|(request, _)| request.page == page);
        native.pending = kept;
        denied
    });
    // Outside the borrow: the denials call into WebKit.
    drop(denied);
}

/// Calls the delegate method as WebKit does, but without an origin, and returns each decision
/// the handler got. Felinaut's method denies such a request; Wry's own method would grant it.
#[cfg(test)]
#[allow(dead_code, reason = "Used by the opt-in native permission test")]
pub fn probe(view: &WKWebView, capture_type: isize) -> Vec<isize> {
    let seen = std::rc::Rc::new(RefCell::new(Vec::new()));
    let log = seen.clone();
    let handler = block2::RcBlock::new(move |decision: isize| log.borrow_mut().push(decision));
    let none: *const AnyObject = std::ptr::null();
    // SAFETY: a live WKWebView and its delegate on the main thread. The arguments have the
    // types of the method; it accepts a null origin and frame, and calls the block at once.
    unsafe {
        let delegate: Option<Retained<AnyObject>> = msg_send![view, UIDelegate];
        if let Some(delegate) = delegate {
            let _: () = msg_send![
                &*delegate,
                webView: view,
                requestMediaCapturePermissionForOrigin: none,
                initiatedByFrame: none,
                type: capture_type,
                decisionHandler: &*handler,
            ];
        }
    }
    seen.take()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, rc::Rc};

    const SITE: &str = "https://meet.example";

    /// A decision handler that records each decision it receives.
    fn reply() -> (Reply, Rc<RefCell<Vec<isize>>>) {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let log = seen.clone();
        let block = block2::RcBlock::new(move |decision: isize| log.borrow_mut().push(decision));
        (Reply(Some(block)), seen)
    }

    fn ask(page: usize, top: &str, frame: &str, capture_type: isize) -> Rc<RefCell<Vec<isize>>> {
        let (reply, seen) = reply();
        request(
            page,
            Some(top.into()),
            Some(frame.into()),
            capture_type,
            reply,
        );
        seen
    }

    #[test]
    fn each_request_gets_exactly_one_answer_and_the_default_is_a_denial() {
        NATIVE.with_borrow_mut(|native| native.pages.extend([1, 2]));

        // An unknown or hidden page, a frame from another site and an unknown capture type
        // are denied at once.
        for seen in [
            ask(9, SITE, SITE, 0),
            ask(1, SITE, "https://ads.example", 0),
            ask(1, SITE, SITE, 7),
        ] {
            assert_eq!(*seen.borrow(), [DENY]);
        }
        assert!(requests().is_empty());

        // A request from a page the user can see waits; nothing is granted meanwhile.
        let allowed = ask(1, SITE, SITE, 2);
        let refused = ask(1, SITE, SITE, 1);
        let other = ask(2, SITE, SITE, 0);
        let waiting = requests();
        assert_eq!(waiting.len(), 3);
        assert_eq!(waiting[0].origin, SITE);
        assert_eq!(waiting[0].kinds, [Kind::Camera, Kind::Microphone]);
        assert!(allowed.borrow().is_empty());

        answer(waiting[0].id, true);
        answer(waiting[0].id, true);
        answer(waiting[0].id, false);
        assert_eq!(*allowed.borrow(), [GRANT], "later answers do nothing");
        answer(waiting[1].id, false);
        assert_eq!(*refused.borrow(), [DENY]);

        // A closed tab or a dropped page denies what still waits, and only its own requests.
        let closed = ask(1, SITE, SITE, 0);
        detach(1);
        assert_eq!(*closed.borrow(), [DENY]);
        assert!(other.borrow().is_empty());
        assert_eq!(requests().len(), 1);
        detach(2);
        assert_eq!(*other.borrow(), [DENY]);
        assert!(requests().is_empty());
        NATIVE.with_borrow_mut(|native| native.pages.push(2));
        assert_eq!(*ask(1, SITE, SITE, 0).borrow(), [DENY], "detached");

        // A page cannot pile up prompts.
        let flood: Vec<_> = (0..MAX_PENDING + 3)
            .map(|_| ask(2, SITE, SITE, 0))
            .collect();
        assert_eq!(requests().len(), MAX_PENDING);
        assert!(
            flood[..MAX_PENDING]
                .iter()
                .all(|seen| seen.borrow().is_empty())
        );
        assert!(
            flood[MAX_PENDING..]
                .iter()
                .all(|seen| *seen.borrow() == [DENY])
        );
        detach(2);
        assert!(flood.iter().all(|seen| *seen.borrow() == [DENY]));
    }
}
