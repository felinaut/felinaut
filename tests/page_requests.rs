//! Run the engine-neutral request ledger without constructing a native page.

#[path = "../src/page/requests.rs"]
mod requests;

type ScriptCallback = Box<dyn Fn(Result<String, requests::ScriptError>) + Send + 'static>;
