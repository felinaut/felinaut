//! HTTP Basic and Digest sign-in.
//!
//! WebKit asks the navigation delegate for a credential and waits for the answer. This module
//! keeps each question until the user answers it. Everything here runs on the main thread. The
//! WebKit callback only queues the question: it never calls into GPUI. The browser's poll shows
//! the prompt (`src/auth_prompt.rs`). The user name and the password are never logged or stored
//! by Felinaut; WebKit keeps the credential until the app quits.

use std::cell::RefCell;

use block2::RcBlock;
use objc2::{class, msg_send, rc::Retained, runtime::AnyObject};
use objc2_foundation::NSString;

/// The most characters of a host or a realm that the prompt shows.
const TEXT_LIMIT: usize = 64;
/// The most questions one page may have open. WebKit gets a cancel for each one above it.
const PAGE_LIMIT: usize = 8;

/// What a server asks, as text that is safe to show. The host and the realm come from the
/// network: they are one line, without control or text-direction characters, and short.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Challenge {
    pub host: String,
    pub port: u16,
    /// The server's name for the protected area. The server chooses this text freely.
    pub realm: String,
    /// The connection uses HTTPS.
    pub secure: bool,
    /// The server refused an earlier answer.
    pub retry: bool,
}

impl Challenge {
    pub fn new(host: &str, port: u16, realm: &str, secure: bool, retry: bool) -> Self {
        let host: Vec<char> = visible(host).collect();
        // The end of a host name tells who owns it, so cut the start.
        let cut = host.len().saturating_sub(TEXT_LIMIT);
        let mut host: String = host[cut..].iter().collect();
        if cut > 0 {
            host.insert(0, '…');
        } else if host.contains(':') {
            // An IPv6 address.
            host = format!("[{host}]");
        }
        let mut realm: String = visible(realm.trim()).take(TEXT_LIMIT + 1).collect();
        if realm.chars().count() > TEXT_LIMIT {
            realm = realm.chars().take(TEXT_LIMIT).chain(['…']).collect();
        }
        Self {
            host,
            port,
            realm,
            secure,
            retry,
        }
    }

    /// The scheme, the host and the port, for example `https://example.com:443`.
    pub fn origin(&self) -> String {
        let scheme = if self.secure { "https" } else { "http" };
        format!("{scheme}://{}:{}", self.host, self.port)
    }
}

fn visible(text: &str) -> impl Iterator<Item = char> {
    text.chars().filter(|character| {
        !character.is_control() && !crate::downloads::is_bidi_control(*character)
    })
}

/// Only a page the user can see may ask, and only for Basic or Digest on the server itself.
/// Each other challenge (a proxy, a certificate, a hidden page) keeps WebKit's own handling.
/// The server must also be the page's own origin: see `same_origin`.
pub fn asks_user(method: &str, proxy: bool, visible: bool) -> bool {
    visible
        && !proxy
        && matches!(
            method,
            "NSURLAuthenticationMethodHTTPBasic" | "NSURLAuthenticationMethodHTTPDigest"
        )
}

/// A page may ask only for its own origin: the scheme, the host and the port of the address
/// that the tab loads or shows. An image, a frame or a fetch from another origin must not put
/// a sign-in form in the place of the page, because the address bar names the page and not
/// the server that asks.
pub fn same_origin(page_url: &str, secure: bool, host: &str, port: u16) -> bool {
    let Ok(url) = url::Url::parse(page_url) else {
        return false;
    };
    url.scheme() == if secure { "https" } else { "http" }
        && url.port_or_known_default() == Some(port)
        && url.host_str().is_some_and(|page| {
            // The URL has brackets around an IPv6 address; the protection space has none.
            page.trim_matches(['[', ']']).eq_ignore_ascii_case(host)
        })
}

struct Waiting<R> {
    id: u64,
    page: usize,
    challenge: Challenge,
    reply: R,
}

/// The open questions, oldest first. `R` is the way to answer one. The list gives each `R`
/// to exactly one caller, who then owns the answer.
pub struct Prompts<R> {
    last_id: u64,
    waiting: Vec<Waiting<R>>,
}

impl<R> Prompts<R> {
    pub const fn new() -> Self {
        Self {
            last_id: 0,
            waiting: Vec::new(),
        }
    }

    /// Adds a question and returns its id. A page with too many gets its `reply` back.
    pub fn push(&mut self, page: usize, challenge: Challenge, reply: R) -> Result<u64, R> {
        let open = self.waiting.iter().filter(|item| item.page == page);
        if open.count() >= PAGE_LIMIT {
            return Err(reply);
        }
        self.last_id += 1;
        self.waiting.push(Waiting {
            id: self.last_id,
            page,
            challenge,
            reply,
        });
        Ok(self.last_id)
    }

    /// The oldest open question of a page.
    pub fn first(&self, page: usize) -> Option<(u64, &Challenge)> {
        let item = self.waiting.iter().find(|item| item.page == page)?;
        Some((item.id, &item.challenge))
    }

    pub fn contains(&self, id: u64) -> bool {
        self.waiting.iter().any(|item| item.id == id)
    }

    /// Removes a question. A second call for the same id returns nothing.
    pub fn take(&mut self, id: u64) -> Option<R> {
        let index = self.waiting.iter().position(|item| item.id == id)?;
        Some(self.waiting.remove(index).reply)
    }

    /// Removes each question of a page.
    pub fn take_page(&mut self, page: usize) -> Vec<R> {
        self.waiting
            .extract_if(.., |item| item.page == page)
            .map(|item| item.reply)
            .collect()
    }
}

/// The completion handler of WebKit: a disposition and a credential.
pub type Handler = block2::Block<dyn Fn(isize, *mut AnyObject)>;

// `NSURLSessionAuthChallengeDisposition`
const USE_CREDENTIAL: isize = 0;
const CANCEL: isize = 2;
const REJECT_PROTECTION_SPACE: isize = 3;
/// `NSURLCredentialPersistenceForSession`: WebKit forgets the credential when the app quits.
const FOR_SESSION: usize = 1;

/// The copied completion handler of one challenge. WebKit raises an exception when the handler
/// is called two times, and also when the handler is released without a call. `send` takes the
/// handler out, so a second call is not possible. `Drop` cancels the challenge if nobody
/// answered: a closed tab, a dropped page, or a page with too many questions.
struct Reply(Option<RcBlock<dyn Fn(isize, *mut AnyObject)>>);

impl Reply {
    fn send(mut self, disposition: isize, credential: *mut AnyObject) {
        if let Some(handler) = self.0.take() {
            handler.call((disposition, credential));
        }
    }
}

impl Drop for Reply {
    fn drop(&mut self) {
        if let Some(handler) = self.0.take() {
            handler.call((CANCEL, std::ptr::null_mut()));
        }
    }
}

thread_local! {
    // `RcBlock` is not `Send`, so the handlers cannot leave the main thread.
    static PROMPTS: RefCell<Prompts<Reply>> = const { RefCell::new(Prompts::new()) };
}

/// Takes a challenge for the user, and returns true. Returns false for a challenge that is not
/// for the user; the caller must then call `handler` itself. `page` is the address of the
/// WKWebView, `visible` says whether it is a page the user can see, and `page_url` is the
/// address that the page loads or shows.
///
/// This runs inside a WebKit callback, so it only queues the question.
pub fn intercept(
    visible: bool,
    page: usize,
    page_url: &str,
    challenge: &AnyObject,
    handler: &Handler,
) -> bool {
    // SAFETY: property reads on a live NSURLAuthenticationChallenge and its protection space,
    // on the main thread, with the types Foundation declares. `realm` and `protocol` can be nil.
    let (method, proxy, host, port, realm, protocol, failures) = unsafe {
        let space: Retained<AnyObject> = msg_send![challenge, protectionSpace];
        let method: Retained<NSString> = msg_send![&*space, authenticationMethod];
        let proxy: bool = msg_send![&*space, isProxy];
        let host: Retained<NSString> = msg_send![&*space, host];
        let port: isize = msg_send![&*space, port];
        let realm: Option<Retained<NSString>> = msg_send![&*space, realm];
        let protocol: Option<Retained<NSString>> = msg_send![&*space, protocol];
        let failures: isize = msg_send![challenge, previousFailureCount];
        (method, proxy, host, port, realm, protocol, failures)
    };
    let Ok(port) = u16::try_from(port) else {
        return false;
    };
    let host = host.to_string();
    let secure = protocol.is_some_and(|protocol| protocol.to_string() == "https");
    if !asks_user(&method.to_string(), proxy, visible)
        || !same_origin(page_url, secure, &host, port)
    {
        return false;
    }
    let challenge = Challenge::new(
        &host,
        port,
        &realm.map(|realm| realm.to_string()).unwrap_or_default(),
        secure,
        failures > 0,
    );
    // The handler can be a stack block that dies when the callback returns. The copy moves it
    // to the heap and keeps it alive until `Reply` calls it.
    let reply = Reply(Some(handler.copy()));
    let refused = PROMPTS.with_borrow_mut(|prompts| prompts.push(page, challenge, reply).err());
    // A refused reply cancels here, after the borrow ended: the handler can run WebKit code,
    // and that code can call this function again.
    drop(refused);
    true
}

/// The oldest open question of a page, with its id.
pub fn waiting(page: usize) -> Option<(u64, Challenge)> {
    PROMPTS.with_borrow(|prompts| {
        prompts
            .first(page)
            .map(|(id, challenge)| (id, challenge.clone()))
    })
}

pub fn is_waiting(id: u64) -> bool {
    PROMPTS.with_borrow(|prompts| prompts.contains(id))
}

/// Answers a question with a user name and a password. Without them the user said no: the
/// request continues without a credential and the page shows the server's own 401 text.
/// An id that was answered or closed before does nothing.
pub fn answer(id: u64, credential: Option<(&str, &str)>) {
    // Take the reply out first. The handler must not run while the list is borrowed.
    let Some(reply) = PROMPTS.with_borrow_mut(|prompts| prompts.take(id)) else {
        return;
    };
    match credential {
        Some((user, password)) => {
            // SAFETY: a class method of NSURLCredential with two live strings and a valid
            // persistence value, on the main thread. `Retained` keeps the credential alive
            // for the call; WebKit retains what it needs.
            let credential: Retained<AnyObject> = unsafe {
                msg_send![
                    class!(NSURLCredential),
                    credentialWithUser: &*NSString::from_str(user),
                    password: &*NSString::from_str(password),
                    persistence: FOR_SESSION,
                ]
            };
            reply.send(USE_CREDENTIAL, Retained::as_ptr(&credential).cast_mut());
        }
        None => reply.send(REJECT_PROTECTION_SPACE, std::ptr::null_mut()),
    }
}

/// Cancels each open question of a page: the page went away or shows a new document.
pub fn close_page(page: usize) {
    let replies = PROMPTS.with_borrow_mut(|prompts| prompts.take_page(page));
    // Each reply cancels its challenge here, after the borrow ended.
    drop(replies);
}

#[cfg(test)]
mod tests {
    use super::{Challenge, PAGE_LIMIT, Prompts, TEXT_LIMIT, asks_user, same_origin};
    use std::{cell::Cell, rc::Rc};

    /// Counts its drops, as the real reply calls WebKit's handler when it drops.
    struct Reply(Rc<Cell<u32>>);
    impl Drop for Reply {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }

    fn challenge(realm: &str) -> Challenge {
        Challenge::new("example.com", 443, realm, true, false)
    }

    #[test]
    fn only_basic_and_digest_on_a_visible_page_ask_the_user() {
        let basic = "NSURLAuthenticationMethodHTTPBasic";
        assert!(asks_user(basic, false, true));
        assert!(asks_user(
            "NSURLAuthenticationMethodHTTPDigest",
            false,
            true
        ));
        assert!(!asks_user(basic, true, true), "a proxy");
        assert!(!asks_user(basic, false, false), "a hidden page");
        for method in [
            "NSURLAuthenticationMethodServerTrust",
            "NSURLAuthenticationMethodNTLM",
            "NSURLAuthenticationMethodNegotiate",
            "NSURLAuthenticationMethodClientCertificate",
            "NSURLAuthenticationMethodHTMLForm",
            "NSURLAuthenticationMethodDefault",
            "",
        ] {
            assert!(!asks_user(method, false, true), "{method}");
        }
    }

    #[test]
    fn only_the_origin_of_the_page_asks_the_user() {
        let page = "https://forum.example/thread?id=1#top";
        assert!(same_origin(page, true, "forum.example", 443));
        assert!(same_origin(page, true, "FORUM.example", 443));
        assert!(!same_origin(page, true, "login-verify.example", 443));
        assert!(!same_origin(page, true, "sub.forum.example", 443));
        assert!(!same_origin(page, true, "example", 443), "a parent domain");
        assert!(!same_origin(page, false, "forum.example", 443), "HTTP");
        assert!(!same_origin(page, true, "forum.example", 8443), "a port");
        assert!(!same_origin(page, false, "forum.example", 80));

        let local = "http://127.0.0.1:8080/";
        assert!(same_origin(local, false, "127.0.0.1", 8080));
        assert!(!same_origin(local, false, "localhost", 8080));
        assert!(same_origin("http://[::1]:8080/", false, "::1", 8080));
        assert!(same_origin("http://example.com/", false, "example.com", 80));
        // The user and the password in a URL are not a part of the host.
        let user = "https://forum.example@evil.example/";
        assert!(!same_origin(user, true, "forum.example", 443));
        assert!(same_origin(user, true, "evil.example", 443));

        for page in ["", "about:blank", "not a url", "file:///etc/hosts"] {
            assert!(!same_origin(page, false, "", 80), "{page:?}");
            assert!(!same_origin(page, true, "example.com", 443), "{page:?}");
        }
    }

    #[test]
    fn network_text_is_one_short_visible_line() {
        let shown = Challenge::new(
            "exam\u{202e}ple.com",
            8080,
            " Staff\r\n\tonly\u{0} ",
            false,
            true,
        );
        assert_eq!(shown.host, "example.com");
        assert_eq!(shown.realm, "Staffonly");
        assert_eq!(shown.origin(), "http://example.com:8080");
        assert!(shown.retry && !shown.secure);

        assert_eq!(challenge("").realm, "", "a server may send no realm");
        let exact = "r".repeat(TEXT_LIMIT);
        assert_eq!(challenge(&exact).realm, exact);
        let long = challenge(&"é".repeat(TEXT_LIMIT + 1)).realm;
        assert_eq!(long.chars().count(), TEXT_LIMIT + 1);
        assert!(long.ends_with('…'));
        assert_eq!(challenge(&"x".repeat(100_000)).realm.len(), TEXT_LIMIT + 3);
    }

    #[test]
    fn a_long_host_keeps_its_end_and_an_ipv6_host_gets_brackets() {
        let host = format!("{}.evil.example", "a".repeat(200));
        let shown = Challenge::new(&host, 443, "", true, false);
        assert_eq!(shown.host.chars().count(), TEXT_LIMIT + 1);
        assert!(shown.host.starts_with('…') && shown.host.ends_with(".evil.example"));
        let local = Challenge::new("::1", 8080, "", false, false);
        assert_eq!(local.origin(), "http://[::1]:8080");
        assert_eq!(challenge("").origin(), "https://example.com:443");
    }

    #[test]
    fn questions_wait_in_order_for_each_page() {
        let mut prompts = Prompts::new();
        assert_eq!(prompts.first(1), None);
        let first = prompts.push(1, challenge("a"), ()).unwrap();
        let other = prompts.push(2, challenge("b"), ()).unwrap();
        let second = prompts.push(1, challenge("c"), ()).unwrap();
        assert_eq!(prompts.first(1), Some((first, &challenge("a"))));
        assert_eq!(prompts.first(2), Some((other, &challenge("b"))));
        assert_eq!(prompts.take(first), Some(()));
        assert_eq!(prompts.first(1), Some((second, &challenge("c"))));
        assert!(prompts.contains(second) && !prompts.contains(first));
        assert_eq!(prompts.first(3), None);
    }

    #[test]
    fn each_reply_leaves_the_list_exactly_one_time() {
        let calls = Rc::new(Cell::new(0));
        let reply = || Reply(calls.clone());
        let mut prompts = Prompts::new();
        let answered = prompts.push(1, challenge(""), reply()).ok().unwrap();
        let closed = prompts.push(1, challenge(""), reply()).ok().unwrap();
        let kept = prompts.push(2, challenge(""), reply()).ok().unwrap();

        // Submit or cancel: the caller gets the reply one time.
        assert!(prompts.take(answered).is_some());
        assert_eq!(calls.get(), 1);
        assert!(prompts.take(answered).is_none(), "a second answer");
        assert!(prompts.take(99).is_none(), "an unknown id");
        assert_eq!(calls.get(), 1);

        // The tab closes: only its own questions end.
        assert_eq!(prompts.take_page(1).len(), 1);
        assert_eq!(calls.get(), 2);
        assert!(!prompts.contains(closed) && prompts.contains(kept));
        assert!(prompts.take_page(1).is_empty());
        assert!(prompts.take(closed).is_none());
        assert_eq!(calls.get(), 2);

        // The list goes away with a question still open.
        drop(prompts);
        assert_eq!(calls.get(), 3);
    }

    #[test]
    fn a_page_with_too_many_questions_gets_the_reply_back() {
        let calls = Rc::new(Cell::new(0));
        let mut prompts = Prompts::new();
        for _ in 0..PAGE_LIMIT {
            assert!(prompts.push(1, challenge(""), Reply(calls.clone())).is_ok());
        }
        assert!(
            prompts
                .push(1, challenge(""), Reply(calls.clone()))
                .is_err()
        );
        assert_eq!(calls.get(), 1, "the refused reply was dropped");
        assert!(prompts.push(2, challenge(""), Reply(calls.clone())).is_ok());
        let first = prompts.first(1).unwrap().0;
        drop(prompts.take(first));
        assert!(prompts.push(1, challenge(""), Reply(calls.clone())).is_ok());
        assert_eq!(calls.get(), 2);
    }
}
