//! JavaScript dialogs: `alert()`, `confirm()` and `prompt()`.
//!
//! This file holds the pure model: the text limits, the queue of one page and the guard that
//! answers each dialog exactly one time. `native` connects the model to WebKit.

use std::collections::VecDeque;

// Spelled out so the native tests, which include this file by path, find it too.
#[path = "dialogs/native.rs"]
pub mod native;

/// The longest message shown, in characters. The rest is cut.
pub const MESSAGE_LIMIT: usize = 2_000;
/// The longest `prompt()` default text and answer, in characters.
pub const TEXT_LIMIT: usize = 2_000;
/// The longest origin shown in a title, in characters.
const ORIGIN_LIMIT: usize = 80;
/// The most dialogs one page can have waiting. A dialog over the limit gets the default answer.
const QUEUE_LIMIT: usize = 8;
/// From this dialog of one document on, the user can stop the dialogs of the document.
const SUPPRESSION_FROM: u64 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Alert,
    Confirm,
    Prompt,
}

/// One dialog a page asked for. All text comes from the page and is cut to the limits here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub kind: Kind,
    /// Scheme, host and port of the frame that asked. Empty when the frame has none.
    pub origin: String,
    pub main_frame: bool,
    pub message: String,
    /// The text `prompt()` starts with.
    pub default_text: String,
}

impl Request {
    pub fn new(
        kind: Kind,
        origin: String,
        main_frame: bool,
        message: &str,
        default_text: &str,
    ) -> Self {
        Self {
            kind,
            origin,
            main_frame,
            message: clip(message, MESSAGE_LIMIT, true),
            default_text: clip(default_text, TEXT_LIMIT, false),
        }
    }

    /// The dialog title. It names the origin of the frame and holds no other text of the page.
    pub fn title(&self) -> String {
        match (self.main_frame, self.origin.is_empty()) {
            (true, false) => format!("{} says", self.origin),
            (true, true) => "This page says".into(),
            (false, false) => format!("An embedded page at {} says", self.origin),
            (false, true) => "An embedded page says".into(),
        }
    }
}

/// The answer to a dialog. The default is the answer of a dialog nobody saw: `alert()` returns,
/// `confirm()` gives `false` and `prompt()` gives `null`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reply {
    pub accepted: bool,
    pub text: String,
}

impl Reply {
    /// What `prompt()` returns: the text, cut to the limit, or nothing for Cancel.
    pub fn prompt_value(&self) -> Option<String> {
        self.accepted.then(|| clip(&self.text, TEXT_LIMIT, false))
    }
}

/// Gives a dialog its answer exactly one time. If it is dropped without an answer, it gives
/// the default answer, so no path can leave a page waiting.
pub struct Responder(Option<Box<dyn FnOnce(Reply)>>);

impl Responder {
    pub fn new(send: impl FnOnce(Reply) + 'static) -> Self {
        Self(Some(Box::new(send)))
    }

    pub fn send(mut self, reply: Reply) {
        if let Some(send) = self.0.take() {
            send(reply);
        }
    }
}

impl Drop for Responder {
    fn drop(&mut self) {
        if let Some(send) = self.0.take() {
            send(Reply::default());
        }
    }
}

/// The dialog a page shows now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shown {
    /// Never used again in the same queue, so an old answer cannot reach a newer dialog.
    pub id: u64,
    pub request: Request,
    /// Offer "Do not let this page show more dialogs".
    pub can_suppress: bool,
}

/// The dialogs of one page, oldest first. `T` is what answers a dialog (a `Responder`).
///
/// The queue never answers a dialog itself. It gives the `T` values back, and the caller
/// answers them after it has released its borrow of the queue.
pub struct Queue<T> {
    waiting: VecDeque<(u64, Request, T)>,
    next_id: u64,
    /// The id of the first dialog of the current document.
    document_start: u64,
    suppressed: bool,
}

impl<T> Default for Queue<T> {
    fn default() -> Self {
        Self {
            waiting: VecDeque::new(),
            next_id: 0,
            document_start: 0,
            suppressed: false,
        }
    }
}

impl<T> Queue<T> {
    /// Adds a dialog. Gives `responder` back if the dialog must get the default answer at once:
    /// the user stopped the dialogs of this document, or too many wait.
    pub fn push(&mut self, request: Request, responder: T) -> Result<(), T> {
        if self.suppressed || self.waiting.len() >= QUEUE_LIMIT {
            return Err(responder);
        }
        self.waiting.push_back((self.next_id, request, responder));
        self.next_id += 1;
        Ok(())
    }

    pub fn shown(&self) -> Option<Shown> {
        let (id, request, _) = self.waiting.front()?;
        Some(Shown {
            id: *id,
            request: request.clone(),
            can_suppress: id - self.document_start + 1 >= SUPPRESSION_FROM,
        })
    }

    /// Takes the shown dialog off the queue when the user answers it. The first value answers
    /// that dialog. With `suppress`, the document can show no more dialogs, and the other
    /// values are the dialogs that waited: they get the default answer.
    /// Gives nothing if `id` is not the shown dialog.
    pub fn answer(&mut self, id: u64, suppress: bool) -> Option<(T, Vec<T>)> {
        if self.waiting.front()?.0 != id {
            return None;
        }
        let (_, _, first) = self.waiting.pop_front()?;
        self.suppressed |= suppress;
        let rest = if suppress { self.dismiss() } else { Vec::new() };
        Some((first, rest))
    }

    /// Removes all dialogs, for the default answer. The user's choice to stop dialogs stays.
    pub fn dismiss(&mut self) -> Vec<T> {
        self.waiting
            .drain(..)
            .map(|(_, _, responder)| responder)
            .collect()
    }

    /// A new document: its dialogs count from one again and are not stopped.
    pub fn reset(&mut self) -> Vec<T> {
        self.suppressed = false;
        self.document_start = self.next_id;
        self.dismiss()
    }
}

/// Scheme, host and port as one origin, or nothing for a frame without a host (`about:blank`,
/// `data:`). A long origin keeps its end, because the end of a host name says who owns it.
pub fn origin(scheme: &str, host: &str, port: u16) -> String {
    let clean = |text: &str| -> String {
        text.chars()
            .filter(|c| !c.is_control() && !c.is_whitespace())
            .collect()
    };
    let (scheme, host) = (clean(scheme), clean(host));
    if scheme.is_empty() || host.is_empty() {
        return String::new();
    }
    let mut rest = host;
    if port != 0 {
        rest.push_str(&format!(":{port}"));
    }
    let scheme: String = scheme.chars().take(16).collect();
    let length = rest.chars().count();
    if length > ORIGIN_LIMIT {
        let tail: String = rest.chars().skip(length - ORIGIN_LIMIT).collect();
        return format!("{scheme}://…{tail}");
    }
    format!("{scheme}://{rest}")
}

/// Cuts page text to `limit` characters and removes control characters. A message keeps its
/// line breaks; text for one line does not.
fn clip(text: &str, limit: usize, lines: bool) -> String {
    let mut clean = text.chars().filter_map(|c| match c {
        '\n' if lines => Some(c),
        '\n' | '\t' => Some(' '),
        c if c.is_control() => None,
        c => Some(c),
    });
    let mut clipped: String = clean.by_ref().take(limit).collect();
    if clean.next().is_some() {
        clipped.push('…');
    }
    clipped
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, rc::Rc};

    fn request(message: &str) -> Request {
        Request::new(
            Kind::Confirm,
            origin("https", "example.com", 0),
            true,
            message,
            "",
        )
    }

    #[test]
    fn dialogs_show_in_order_and_each_is_answered_once() {
        let mut queue = Queue::default();
        assert_eq!(queue.shown(), None);
        assert!(queue.answer(0, false).is_none());
        queue.push(request("first"), 'a').unwrap();
        queue.push(request("second"), 'b').unwrap();
        let first = queue.shown().unwrap();
        assert_eq!(first.request.message, "first");
        // An answer for a dialog that is not on show changes nothing.
        assert!(queue.answer(first.id + 1, true).is_none());
        assert_eq!(queue.answer(first.id, false), Some(('a', vec![])));
        assert!(queue.answer(first.id, false).is_none());
        let second = queue.shown().unwrap();
        assert_eq!(second.request.message, "second");
        assert_eq!(queue.answer(second.id, false), Some(('b', vec![])));
        assert_eq!(queue.shown(), None);
    }

    #[test]
    fn suppression_is_offered_from_the_second_dialog_and_ends_with_the_document() {
        let mut queue = Queue::default();
        queue.push(request("1"), 1).unwrap();
        let first = queue.shown().unwrap();
        assert!(!first.can_suppress);
        queue.answer(first.id, false).unwrap();
        queue.push(request("2"), 2).unwrap();
        queue.push(request("3"), 3).unwrap();
        let second = queue.shown().unwrap();
        assert!(second.can_suppress);
        // The dialog that waited gets the default answer, and so does each later one.
        assert_eq!(queue.answer(second.id, true), Some((2, vec![3])));
        assert_eq!(queue.shown(), None);
        assert_eq!(queue.push(request("4"), 4), Err(4));
        // A dismissal (the user reloads) does not give the page its dialogs back.
        assert!(queue.dismiss().is_empty());
        assert_eq!(queue.push(request("5"), 5), Err(5));
        // A new document starts again, without the offer on its first dialog.
        assert!(queue.reset().is_empty());
        queue.push(request("6"), 6).unwrap();
        let next = queue.shown().unwrap();
        assert!(!next.can_suppress);
        assert!(next.id > second.id, "ids are never used again");
    }

    #[test]
    fn navigation_and_a_full_queue_give_default_answers() {
        let mut queue = Queue::default();
        for index in 0..QUEUE_LIMIT {
            queue.push(request("wait"), index).unwrap();
        }
        assert_eq!(queue.push(request("too many"), 99), Err(99));
        assert_eq!(queue.reset(), (0..QUEUE_LIMIT).collect::<Vec<_>>());
        assert_eq!(queue.shown(), None);
        queue.push(request("next"), 100).unwrap();
        assert_eq!(queue.dismiss(), vec![100]);
    }

    #[test]
    fn a_responder_answers_exactly_once_also_when_dropped() {
        let replies = Rc::new(RefCell::new(Vec::new()));
        let responder = || {
            let replies = replies.clone();
            Responder::new(move |reply| replies.borrow_mut().push(reply))
        };
        let yes = Reply {
            accepted: true,
            text: "name".into(),
        };
        responder().send(yes.clone());
        // Dropped: the tab closed, the page navigated or the user stopped the dialogs.
        drop(responder());
        let mut queue = Queue::default();
        queue.push(request("closed with its tab"), responder()).ok();
        drop(queue);
        assert_eq!(*replies.borrow(), [yes, Reply::default(), Reply::default()]);
    }

    #[test]
    fn replies_map_to_javascript_values() {
        assert_eq!(Reply::default().prompt_value(), None);
        let reply = |text: &str| Reply {
            accepted: true,
            text: text.into(),
        };
        assert_eq!(reply("").prompt_value(), Some(String::new()));
        assert_eq!(reply("a\nb").prompt_value().as_deref(), Some("a b"));
        let long = reply(&"é".repeat(TEXT_LIMIT + 1)).prompt_value().unwrap();
        assert_eq!(long.chars().count(), TEXT_LIMIT + 1);
        assert!(long.ends_with('…'));
    }

    #[test]
    fn page_text_is_cut_and_cleaned() {
        let exact = "x".repeat(MESSAGE_LIMIT);
        assert_eq!(request(&exact).message, exact);
        let cut = request(&"界".repeat(MESSAGE_LIMIT + 5)).message;
        assert_eq!(cut.chars().count(), MESSAGE_LIMIT + 1);
        assert!(cut.ends_with('…'));
        assert_eq!(request("a\r\n\tb\u{7}\u{1b}[1m").message, "a\n b[1m");
        assert_eq!(request("").message, "");
        let prompt = Request::new(Kind::Prompt, String::new(), true, "", "one\ntwo");
        assert_eq!(prompt.default_text, "one two");
    }

    #[test]
    fn titles_name_only_the_origin() {
        assert_eq!(origin("https", "example.com", 0), "https://example.com");
        assert_eq!(origin("http", "127.0.0.1", 8080), "http://127.0.0.1:8080");
        assert_eq!(origin("", "example.com", 0), "");
        assert_eq!(origin("about", "", 0), "");
        assert_eq!(origin("https", "a b\n.com", 0), "https://ab.com");
        // A long host keeps the end, which names its owner.
        let long = origin("https", &format!("{}.evil.example", "a".repeat(300)), 8443);
        assert!(long.starts_with("https://…aaa"), "{long}");
        assert!(long.ends_with(".evil.example:8443"), "{long}");
        assert_eq!(long.chars().count(), "https://…".len() - 2 + ORIGIN_LIMIT);

        let title = |origin: &str, main_frame| {
            Request::new(Kind::Alert, origin.into(), main_frame, "Felinaut says", "").title()
        };
        assert_eq!(
            title("https://example.com", true),
            "https://example.com says"
        );
        assert_eq!(title("", true), "This page says");
        assert_eq!(
            title("https://ads.example", false),
            "An embedded page at https://ads.example says"
        );
        assert_eq!(title("", false), "An embedded page says");
    }
}
