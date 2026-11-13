//! A quick check that a proxy answers as a SOCKS5 server and accepts the saved credentials.
//! It opens one TCP connection to the proxy itself and asks it to reach nothing.

use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream, ToSocketAddrs},
    time::Duration,
};

use super::model::Proxy;

const VERSION: u8 = 5;
const NO_AUTH: u8 = 0;
const USER_PASS: u8 = 2;
const NO_ACCEPTABLE: u8 = 0xff;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Probe {
    /// The proxy speaks SOCKS5 and accepted the handshake.
    Ready,
    Unreachable,
    NotSocks5,
    /// The proxy wants a user name and password, and none is saved.
    NeedsCredentials,
    CredentialsRejected,
    /// The proxy refused every method offered.
    NoMethod,
}

impl Probe {
    pub fn message(self) -> Option<&'static str> {
        match self {
            Self::Ready => None,
            Self::Unreachable => Some("The proxy does not answer. Pages will not load."),
            Self::NotSocks5 => Some("That server is not a SOCKS5 proxy. Pages will not load."),
            Self::NeedsCredentials => Some("The proxy needs a user name and password."),
            Self::CredentialsRejected => Some("The proxy rejected the user name or password."),
            Self::NoMethod => Some("The proxy refused every sign-in method Felinaut offers."),
        }
    }
}

/// The method selection message (RFC 1928 §3).
fn greeting(credentials: bool) -> Vec<u8> {
    if credentials {
        vec![VERSION, 2, NO_AUTH, USER_PASS]
    } else {
        vec![VERSION, 1, NO_AUTH]
    }
}

/// What the proxy chose; `Ok(true)` means it wants user name and password.
fn method(reply: [u8; 2], credentials: bool) -> Result<bool, Probe> {
    match reply {
        [VERSION, NO_AUTH] => Ok(false),
        [VERSION, USER_PASS] if credentials => Ok(true),
        [VERSION, USER_PASS] => Err(Probe::NeedsCredentials),
        [VERSION, NO_ACCEPTABLE] if !credentials => Err(Probe::NeedsCredentials),
        [VERSION, _] => Err(Probe::NoMethod),
        _ => Err(Probe::NotSocks5),
    }
}

/// The user name and password request (RFC 1929 §2). Validation keeps both to 255 bytes.
fn credentials(username: &str, password: &str) -> Option<Vec<u8>> {
    let user = u8::try_from(username.len()).ok()?;
    let pass = u8::try_from(password.len()).ok()?;
    let mut message = vec![1, user];
    message.extend_from_slice(username.as_bytes());
    message.push(pass);
    message.extend_from_slice(password.as_bytes());
    Some(message)
}

fn verdict(reply: [u8; 2]) -> Probe {
    match reply {
        [1, 0] => Probe::Ready,
        [1, _] => Probe::CredentialsRejected,
        _ => Probe::NotSocks5,
    }
}

/// Run the handshake with `stream`; `login` is the user name and password, if saved.
fn handshake(stream: &mut (impl Read + Write), login: Option<(&str, &str)>) -> Probe {
    let mut reply = [0; 2];
    if stream.write_all(&greeting(login.is_some())).is_err() {
        return Probe::Unreachable;
    }
    if stream.read_exact(&mut reply).is_err() {
        return Probe::NotSocks5;
    }
    let needs_login = match method(reply, login.is_some()) {
        Ok(needs) => needs,
        Err(probe) => return probe,
    };
    let Some((user, pass)) = login.filter(|_| needs_login) else {
        return Probe::Ready;
    };
    let Some(message) = credentials(user, pass) else {
        return Probe::CredentialsRejected;
    };
    if stream.write_all(&message).is_err() || stream.read_exact(&mut reply).is_err() {
        return Probe::CredentialsRejected;
    }
    verdict(reply)
}

/// Blocking; run it off the UI thread. Every step is bounded by `timeout`.
pub fn probe(proxy: &Proxy, password: Option<&str>, timeout: Duration) -> Probe {
    let addresses: Vec<SocketAddr> = match (proxy.host.as_str(), proxy.port).to_socket_addrs() {
        Ok(addresses) => addresses.take(4).collect(),
        Err(_) => return Probe::Unreachable,
    };
    let Some(mut stream) = addresses
        .iter()
        .find_map(|address| TcpStream::connect_timeout(address, timeout).ok())
    else {
        return Probe::Unreachable;
    };
    if stream.set_read_timeout(Some(timeout)).is_err()
        || stream.set_write_timeout(Some(timeout)).is_err()
    {
        return Probe::Unreachable;
    }
    let login = proxy
        .username
        .as_deref()
        .map(|user| (user, password.unwrap_or("")));
    handshake(&mut stream, login)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::model::{Draft, validate};
    use std::{net::TcpListener, thread};

    #[test]
    fn greeting_offers_password_auth_only_with_credentials() {
        assert_eq!(greeting(false), [5, 1, 0]);
        assert_eq!(greeting(true), [5, 2, 0, 2]);
    }

    #[test]
    fn method_replies_are_interpreted() {
        assert_eq!(method([5, 0], false), Ok(false));
        assert_eq!(method([5, 0], true), Ok(false));
        assert_eq!(method([5, 2], true), Ok(true));
        assert_eq!(method([5, 2], false), Err(Probe::NeedsCredentials));
        assert_eq!(method([5, 0xff], false), Err(Probe::NeedsCredentials));
        assert_eq!(method([5, 0xff], true), Err(Probe::NoMethod));
        assert_eq!(method([5, 1], true), Err(Probe::NoMethod));
        for reply in [[4, 0], [0x48, 0x54], [0, 0]] {
            assert_eq!(method(reply, true), Err(Probe::NotSocks5), "{reply:?}");
        }
    }

    #[test]
    fn credential_messages_carry_lengths_and_refuse_oversized_fields() {
        assert_eq!(
            credentials("al", "pw!").unwrap(),
            [1, 2, b'a', b'l', 3, b'p', b'w', b'!']
        );
        assert_eq!(credentials("", "").unwrap(), [1, 0, 0]);
        let max = "x".repeat(255);
        assert_eq!(credentials(&max, &max).unwrap().len(), 3 + 2 * 255);
        assert_eq!(credentials(&"x".repeat(256), ""), None);
        assert_eq!(credentials("", &"x".repeat(256)), None);
    }

    #[test]
    fn auth_verdicts() {
        assert_eq!(verdict([1, 0]), Probe::Ready);
        assert_eq!(verdict([1, 1]), Probe::CredentialsRejected);
        assert_eq!(verdict([5, 0]), Probe::NotSocks5);
    }

    #[test]
    fn every_failure_has_a_message() {
        assert_eq!(Probe::Ready.message(), None);
        for probe in [
            Probe::Unreachable,
            Probe::NotSocks5,
            Probe::NeedsCredentials,
            Probe::CredentialsRejected,
            Probe::NoMethod,
        ] {
            assert!(probe.message().is_some(), "{probe:?}");
        }
    }

    /// A loopback server that reads what a client sends and answers with `script`.
    fn server(script: Vec<(usize, Vec<u8>)>) -> (Proxy, thread::JoinHandle<Vec<u8>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut received = Vec::new();
            for (expect, answer) in script {
                let mut buffer = vec![0; expect];
                if stream.read_exact(&mut buffer).is_err() {
                    break;
                }
                received.extend(buffer);
                stream.write_all(&answer).unwrap();
            }
            received
        });
        (loopback(port), handle)
    }

    fn loopback(port: u16) -> Proxy {
        validate(
            &Draft {
                host: "127.0.0.1",
                port: &port.to_string(),
                ..Draft::default()
            },
            &[],
        )
        .unwrap()
    }

    const TIMEOUT: Duration = Duration::from_secs(5);

    #[test]
    fn open_proxy_is_ready() {
        let (proxy, server) = server(vec![(3, vec![5, 0])]);
        assert_eq!(probe(&proxy, None, TIMEOUT), Probe::Ready);
        assert_eq!(server.join().unwrap(), [5, 1, 0]);
    }

    #[test]
    fn password_proxy_checks_the_credentials() {
        for (answer, expected) in [(1, Probe::Ready), (0, Probe::CredentialsRejected)] {
            let status = if answer == 1 { 0 } else { 1 };
            let (mut proxy, server) = server(vec![(4, vec![5, 2]), (10, vec![1, status])]);
            proxy.username = Some("bob".into());
            assert_eq!(probe(&proxy, Some("pass"), TIMEOUT), expected);
            assert_eq!(
                server.join().unwrap(),
                [
                    5, 2, 0, 2, 1, 3, b'b', b'o', b'b', 4, b'p', b'a', b's', b's'
                ]
            );
        }
    }

    #[test]
    fn password_proxy_without_saved_credentials_says_so() {
        let (proxy, server) = server(vec![(3, vec![5, 0xff])]);
        assert_eq!(probe(&proxy, None, TIMEOUT), Probe::NeedsCredentials);
        server.join().unwrap();
    }

    #[test]
    fn other_servers_are_not_socks5() {
        let (proxy, handle) = server(vec![(3, b"HTTP/1.1 400".to_vec())]);
        assert_eq!(probe(&proxy, None, TIMEOUT), Probe::NotSocks5);
        handle.join().unwrap();
        // A server that hangs up without answering.
        let (proxy, handle) = server(vec![]);
        assert_eq!(probe(&proxy, None, TIMEOUT), Probe::NotSocks5);
        handle.join().unwrap();
    }

    #[test]
    fn silent_servers_time_out() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let proxy = loopback(listener.local_addr().unwrap().port());
        // Accepted by the kernel's backlog, but nothing ever answers.
        assert_eq!(
            probe(&proxy, None, Duration::from_millis(200)),
            Probe::NotSocks5
        );
        drop(listener);
    }

    #[test]
    fn closed_ports_are_unreachable() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let proxy = loopback(port);
        assert_eq!(probe(&proxy, None, TIMEOUT), Probe::Unreachable);
    }
}
