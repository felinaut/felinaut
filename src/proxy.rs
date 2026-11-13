//! SOCKS5 proxies for every page: the saved list, the one in use, and its live status.
//!
//! The list lives in `~/Library/Application Support/Felifox/proxies.json` without passwords;
//! passwords are kept in the login Keychain. Switching applies to every tab, the hidden
//! live-folder page and downloads at once, because they share WebKit's default data store.

#[cfg(target_os = "macos")]
mod keychain;
mod model;
#[cfg(target_os = "macos")]
mod network;
mod probe;

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use gpui::{App, Global};

pub use model::{Draft, Proxy, ProxyError, ProxyList};
use probe::Probe;

/// How long the reachability check waits for each step.
const PROBE_TIMEOUT: Duration = Duration::from_secs(6);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// No proxy: pages connect directly.
    Direct,
    /// Pages go to the proxy; Felinaut is still reading the Keychain or checking it answers.
    Checking,
    /// The proxy answered the SOCKS5 handshake.
    Ready,
    /// Pages go to the proxy, but it will not carry them; they fail instead of going direct.
    Problem(&'static str),
    /// This macOS cannot send pages through a proxy.
    Unsupported,
}

pub struct Proxies {
    pub list: ProxyList,
    pub status: Status,
    /// Whether this macOS can route pages through a proxy (macOS 14 and later).
    pub supported: bool,
    /// Ignores Keychain reads and checks for a proxy that is no longer in use.
    generation: u64,
    /// Counts proxies whose password arrived after pages may already have tried to load
    /// without it; the browser reloads a page that failed meanwhile.
    pub credentials_applied: u64,
}
impl Global for Proxies {}

#[cfg(test)]
impl Proxies {
    pub fn for_test(list: ProxyList, status: Status) -> Self {
        Self {
            list,
            status,
            supported: true,
            generation: 0,
            credentials_applied: 0,
        }
    }
}

fn file_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(Path::new(&home).join("Library/Application Support/Felifox/proxies.json"))
}

/// Load the saved proxies and route pages through the chosen one. Call before any page loads.
pub fn init(cx: &mut App) {
    let list = file_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|text| ProxyList::parse(&text))
        .unwrap_or_default();
    cx.set_global(Proxies {
        list,
        status: Status::Direct,
        supported: native::supported(),
        generation: 0,
        credentials_applied: 0,
    });
    apply(None, cx);
}

/// Use saved proxy `index`, or connect directly with `None`.
pub fn connect(index: Option<usize>, cx: &mut App) {
    let state = cx.global_mut::<Proxies>();
    if index.is_some() && !state.supported {
        return;
    }
    if state.list.select(index) {
        save(cx);
        apply(None, cx);
    }
}

/// Check the proxy in use again, for example after starting it. Routing stays as it is, so
/// this never makes the browser retry pages.
pub fn recheck(cx: &mut App) {
    if cx.global::<Proxies>().list.active.is_some() {
        update(None, false, cx);
    }
}

/// Route pages for the current choice, then check it.
fn apply(password: Option<String>, cx: &mut App) {
    update(password, true, cx);
}

/// Save a new proxy from the form and switch to it.
pub fn add(draft: &Draft, cx: &mut App) -> Result<(), ProxyError> {
    let state = cx.global_mut::<Proxies>();
    let proxy = model::validate(draft, &state.list.proxies)?;
    let password = proxy.username.as_ref().map(|_| draft.password.to_owned());
    if let (Some(account), Some(password)) = (proxy.keychain_account(), password.clone()) {
        cx.background_executor()
            .spawn(async move {
                if native::store_password(&account, &password).is_err() {
                    eprintln!("Could not save the proxy password in the Keychain.");
                }
            })
            .detach();
    }
    let state = cx.global_mut::<Proxies>();
    let index = state.list.add(proxy);
    if state.supported {
        state.list.select(Some(index));
    }
    save(cx);
    apply(password, cx);
    Ok(())
}

/// Forget saved proxy `index` and its password. Removing the proxy in use goes direct.
pub fn remove(index: usize, cx: &mut App) {
    let state = cx.global_mut::<Proxies>();
    let was_active = state.list.active == Some(index);
    let Some(proxy) = state.list.remove(index) else {
        return;
    };
    save(cx);
    if let Some(account) = proxy.keychain_account() {
        cx.background_executor()
            .spawn(async move {
                if native::delete_password(&account).is_err() {
                    eprintln!("Could not remove the proxy password from the Keychain.");
                }
            })
            .detach();
    }
    if was_active {
        apply(None, cx);
    }
}

fn save(cx: &App) {
    // Unit tests drive these functions; they must not touch the user's file.
    if cfg!(test) {
        return;
    }
    let Some(path) = file_path() else {
        return;
    };
    let text = cx.global::<Proxies>().list.serialize();
    cx.background_executor()
        .spawn(async move {
            let tmp = path.with_extension("json.tmp");
            let result = path
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| std::fs::write(&tmp, text))
                .and_then(|()| std::fs::rename(&tmp, &path));
            if let Err(error) = result {
                eprintln!("Could not save proxies: {error}");
            }
        })
        .detach();
}

/// The status to show once the check has run.
fn status_after(probe: Probe) -> Status {
    match probe.message() {
        None => Status::Ready,
        Some(message) => Status::Problem(message),
    }
}

/// Route pages for the current choice when `route` is set, and check that the proxy answers.
/// `password` is the proxy's password when the caller already has it; otherwise it is read
/// from the Keychain in the background.
fn update(password: Option<String>, route: bool, cx: &mut App) {
    let state = cx.global_mut::<Proxies>();
    state.generation += 1;
    let generation = state.generation;
    let Some(proxy) = state.list.active_proxy().cloned() else {
        if state.supported && native::apply(None).is_err() {
            eprintln!("Could not turn the proxy off.");
        }
        state.status = Status::Direct;
        return;
    };
    if !state.supported {
        state.status = Status::Unsupported;
        return;
    }
    // Until the password arrives, pages go to the proxy without it and fail; never direct.
    if route && native::apply(Some((&proxy, password.as_deref()))).is_err() {
        state.status = Status::Problem("Felinaut could not set up this proxy.");
        return;
    }
    state.status = Status::Checking;
    let account = proxy.keychain_account().filter(|_| password.is_none());

    cx.spawn(async move |cx| {
        let current = |cx: &App| cx.global::<Proxies>().generation == generation;
        let mut password = password;
        if let Some(account) = account {
            let loaded = cx
                .background_executor()
                .spawn(async move { native::load_password(&account) })
                .await;
            let still_current = cx.update(|cx| {
                if !current(cx) {
                    return false;
                }
                let state = cx.global_mut::<Proxies>();
                match &loaded {
                    Ok(Some(saved)) => {
                        if route && native::apply(Some((&proxy, Some(saved)))).is_ok() {
                            state.credentials_applied += 1;
                        }
                    }
                    Ok(None) => {
                        state.status = Status::Problem(
                            "No password for this proxy is saved. Remove it and add it again.",
                        );
                        return false;
                    }
                    Err(()) => {
                        state.status =
                            Status::Problem("Felinaut could not read the proxy password.");
                        return false;
                    }
                }
                true
            });
            if !matches!(still_current, Ok(true)) {
                return;
            }
            password = loaded.ok().flatten();
        }
        let checked = proxy.clone();
        let probe = cx
            .background_executor()
            .spawn(async move { probe::probe(&checked, password.as_deref(), PROBE_TIMEOUT) })
            .await;
        let _ = cx.update(|cx| {
            if current(cx) {
                cx.global_mut::<Proxies>().status = status_after(probe);
            }
        });
    })
    .detach();
}

/// Native calls, with stand-ins where macOS APIs are missing.
mod native {
    use super::Proxy;

    #[cfg(target_os = "macos")]
    pub fn supported() -> bool {
        super::network::supported()
    }
    #[cfg(target_os = "macos")]
    pub fn apply(proxy: Option<(&Proxy, Option<&str>)>) -> Result<(), ()> {
        super::network::apply(proxy).map_err(|_| ())
    }
    #[cfg(target_os = "macos")]
    pub fn store_password(account: &str, password: &str) -> Result<(), ()> {
        super::keychain::store(account, password).map_err(|_| ())
    }
    #[cfg(target_os = "macos")]
    pub fn load_password(account: &str) -> Result<Option<String>, ()> {
        super::keychain::load(account).map_err(|_| ())
    }
    #[cfg(target_os = "macos")]
    pub fn delete_password(account: &str) -> Result<(), ()> {
        super::keychain::delete(account).map_err(|_| ())
    }

    #[cfg(not(target_os = "macos"))]
    pub fn supported() -> bool {
        false
    }
    #[cfg(not(target_os = "macos"))]
    pub fn apply(_: Option<(&Proxy, Option<&str>)>) -> Result<(), ()> {
        Err(())
    }
    #[cfg(not(target_os = "macos"))]
    pub fn store_password(_: &str, _: &str) -> Result<(), ()> {
        Err(())
    }
    #[cfg(not(target_os = "macos"))]
    pub fn load_password(_: &str) -> Result<Option<String>, ()> {
        Err(())
    }
    #[cfg(not(target_os = "macos"))]
    pub fn delete_password(_: &str) -> Result<(), ()> {
        Err(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_successful_check_is_ready_and_failures_explain_themselves() {
        assert_eq!(status_after(Probe::Ready), Status::Ready);
        for probe in [
            Probe::Unreachable,
            Probe::NotSocks5,
            Probe::NeedsCredentials,
            Probe::CredentialsRejected,
            Probe::NoMethod,
        ] {
            assert!(
                matches!(status_after(probe), Status::Problem(_)),
                "{probe:?}"
            );
        }
    }
}
