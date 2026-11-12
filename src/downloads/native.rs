//! The WebKit side of downloads.
//!
//! Wry's own download handlers report neither progress nor the `WKDownload`, so a download
//! could not be cancelled. Felinaut therefore becomes the `WKDownloadDelegate` itself: the two
//! `didBecomeDownload` methods of Wry's navigation delegate are replaced by ones that hand the
//! download to this module. Everything here runs on the main thread, inside WebKit callbacks
//! or the browser's poll, and only queues events: it never calls into GPUI.

use std::{cell::RefCell, path::PathBuf};

use objc2::{
    MainThreadMarker, MainThreadOnly, Message, define_class, msg_send,
    rc::Retained,
    runtime::{AnyObject, Imp, NSObject, ProtocolObject, Sel},
    sel,
};
use objc2_foundation::{
    NSData, NSError, NSObjectProtocol, NSProgressReporting, NSString, NSURL, NSURLResponse,
};
use objc2_web_kit::{WKDownload, WKDownloadDelegate, WKWebView};

use super::{Event, destination, folder};

struct Active {
    key: u64,
    download: Retained<WKDownload>,
    path: PathBuf,
    /// The progress last reported, so the poll only queues changes.
    seen: (u64, Option<u64>),
}

#[derive(Default)]
struct Native {
    delegate: Option<Retained<Delegate>>,
    /// The pages that may download, by their WKWebView's address.
    pages: Vec<usize>,
    active: Vec<Active>,
    events: Vec<Event>,
    last_key: u64,
}

thread_local! {
    static NATIVE: RefCell<Native> = RefCell::default();
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and the class has no ivars or `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "FelinautDownloadDelegate"]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    // SAFETY: the signatures are those of `objc2_web_kit::WKDownloadDelegate`.
    unsafe impl WKDownloadDelegate for Delegate {
        #[unsafe(method(download:decideDestinationUsingResponse:suggestedFilename:completionHandler:))]
        fn decide_destination(
            &self,
            download: &WKDownload,
            response: &NSURLResponse,
            suggested: &NSString,
            handler: &block2::DynBlock<dyn Fn(*mut NSURL)>,
        ) {
            // WebKit expects exactly one call; a null URL cancels the download.
            match start(download, response, &suggested.to_string()) {
                Some(file) => handler.call((Retained::as_ptr(&file).cast_mut(),)),
                None => handler.call((std::ptr::null_mut(),)),
            }
        }

        #[unsafe(method(downloadDidFinish:))]
        fn did_finish(&self, download: &WKDownload) {
            settle(download, true);
        }

        #[unsafe(method(download:didFailWithError:resumeData:))]
        fn did_fail(&self, download: &WKDownload, _: &NSError, _: Option<&NSData>) {
            settle(download, false);
        }
    }
);

/// Routes downloads of `view` here. Only a page the user can see (`allowed`) may save files;
/// downloads from any other page are cancelled. Call `detach` when an allowed page goes away.
pub fn attach(view: &WKWebView, allowed: bool) {
    type Callback = unsafe extern "C-unwind" fn(
        *mut AnyObject,
        Sel,
        *mut AnyObject,
        *mut AnyObject,
        *mut WKDownload,
    );
    unsafe extern "C-unwind" fn became_download(
        _: *mut AnyObject,
        _: Sel,
        webview: *mut AnyObject,
        _: *mut AnyObject,
        download: *mut WKDownload,
    ) {
        // SAFETY: WebKit passes a live download, on the main thread.
        if let Some(download) = unsafe { download.as_ref() } {
            adopt(webview as usize, download);
        }
    }

    // SAFETY: a property read on a live WKWebView, on the main thread.
    let delegate: Option<Retained<AnyObject>> = unsafe { msg_send![view, navigationDelegate] };
    let Some(delegate) = delegate else {
        return;
    };
    // SAFETY: `class` is Wry's navigation delegate class, which implements both selectors by
    // setting Wry's download delegate. The replacement has the same type encoding: void, self,
    // _cmd and three objects. Replacing again for a later page changes nothing.
    unsafe {
        let class = delegate.class() as *const _ as *mut _;
        let imp = std::mem::transmute::<Callback, Imp>(became_download);
        for selector in [
            sel!(webView:navigationAction:didBecomeDownload:),
            sel!(webView:navigationResponse:didBecomeDownload:),
        ] {
            objc2::ffi::class_replaceMethod(class, selector, imp, c"v@:@@@".as_ptr());
        }
    }
    if allowed {
        let key = view as *const WKWebView as usize;
        NATIVE.with_borrow_mut(|native| native.pages.push(key));
    }
}

/// Forgets a page given to `attach`, by its WKWebView's address. Its downloads carry on.
pub fn detach(page: usize) {
    NATIVE.with_borrow_mut(|native| native.pages.retain(|known| *known != page));
}

fn adopt(page: usize, download: &WKDownload) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let delegate = NATIVE.with_borrow_mut(|native| {
        native.pages.contains(&page).then(|| {
            native
                .delegate
                .get_or_insert_with(|| {
                    // SAFETY: a plain NSObject subclass without ivars.
                    unsafe { msg_send![Delegate::alloc(mtm), init] }
                })
                .clone()
        })
    });
    // SAFETY: a live download on the main thread. It holds its delegate weakly; the
    // thread-local keeps ours alive for the life of the app.
    unsafe {
        match delegate {
            Some(delegate) => download.setDelegate(Some(ProtocolObject::from_ref(&*delegate))),
            // A page the user cannot see (a live folder's reader) must not save files.
            None => download.cancel(None),
        }
    }
}

/// Picks the file for a new download and queues its `Started` event.
fn start(
    download: &WKDownload,
    response: &NSURLResponse,
    suggested: &str,
) -> Option<Retained<NSURL>> {
    // SAFETY: a property read on a live download, on the main thread.
    let url = unsafe { download.originalRequest() }
        .and_then(|request| request.URL())
        .and_then(|url| url.absoluteString())
        .map(|url| url.to_string())
        .unwrap_or_default();
    // -1 when the server did not say.
    let total = u64::try_from(response.expectedContentLength()).ok();
    NATIVE.with_borrow_mut(|native| {
        // A few `lstat` calls: WebKit needs the answer in this callback. A name another
        // running download was given counts as taken before its file exists.
        let path = destination(&folder()?, suggested, |path| {
            path.symlink_metadata().is_ok() || native.active.iter().any(|other| other.path == path)
        });
        let Some(path) = path else {
            eprintln!("Could not choose a file in the Downloads folder");
            return None;
        };
        let file = NSURL::fileURLWithPath_isDirectory(&NSString::from_str(path.to_str()?), false);
        native.last_key += 1;
        let key = native.last_key;
        native.active.push(Active {
            key,
            download: download.retain(),
            path: path.clone(),
            seen: (0, total),
        });
        native.events.push(Event::Started {
            key,
            url,
            path,
            total,
        });
        Some(file)
    })
}

fn settle(download: &WKDownload, finished: bool) {
    NATIVE.with_borrow_mut(|native| {
        // A download cancelled from the list is no longer here; WebKit's report is dropped.
        let Some(index) = native
            .active
            .iter()
            .position(|active| std::ptr::eq(&*active.download, download))
        else {
            return;
        };
        let active = native.active.remove(index);
        native.events.push(if finished {
            Event::Finished {
                key: active.key,
                received: progress(&active.download).0,
            }
        } else {
            discard(active.path);
            Event::Failed { key: active.key }
        });
    });
}

/// Deletes what a failed or cancelled download wrote: WebKit leaves the partial file under
/// the final name, where it would pass for the whole file. The name was free when this
/// download got it, so the file holds nothing else. Off the main thread, like all disk writes.
fn discard(path: PathBuf) {
    std::thread::spawn(move || {
        let _ = std::fs::remove_file(path);
    });
}

/// Bytes written and, when WebKit knows it, the file's size.
fn progress(download: &WKDownload) -> (u64, Option<u64>) {
    let progress = download.progress();
    (
        u64::try_from(progress.completedUnitCount()).unwrap_or(0),
        u64::try_from(progress.totalUnitCount())
            .ok()
            .filter(|total| *total > 0),
    )
}

/// What happened since the last call, with the progress of running downloads that moved.
/// The browser polls this on the UI thread.
pub fn drain() -> Vec<Event> {
    NATIVE.with_borrow_mut(|native| {
        let mut events = std::mem::take(&mut native.events);
        for active in &mut native.active {
            let (received, total) = progress(&active.download);
            let now = (received, total.or(active.seen.1));
            if now != active.seen {
                active.seen = now;
                events.push(Event::Progress {
                    key: active.key,
                    received,
                    total,
                });
            }
        }
        events
    })
}

/// Stops a running download and deletes its partial file once WebKit has let go of it.
pub fn cancel(key: u64) {
    let active = NATIVE.with_borrow_mut(|native| {
        let index = native.active.iter().position(|active| active.key == key)?;
        Some(native.active.remove(index))
    });
    if let Some(Active { download, path, .. }) = active {
        // The resume data is not kept: a cancelled download starts over.
        let stopped = block2::RcBlock::new(move |_: *mut NSData| discard(path.clone()));
        // SAFETY: a live download on the main thread; WebKit copies the block and calls it
        // once, on the main thread.
        unsafe { download.cancel(Some(&stopped)) };
    }
}
