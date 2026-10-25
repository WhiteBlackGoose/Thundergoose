//! Opening authenticated IMAP sessions.

use std::net::{TcpStream, ToSocketAddrs};
use std::sync::OnceLock;
use std::time::Duration;

use imap::{Connection, Session};
use rustls_connector::RustlsConnector;

use crate::config::{Account, Security};
use crate::creds::{CredError, Creds, Login};

pub type Imap = Session<Connection>;

#[derive(Debug)]
pub enum ConnError {
    Auth(String),
    NeedsLogin(String),
    Net(String),
}

impl std::fmt::Display for ConnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnError::Auth(m) | ConnError::NeedsLogin(m) | ConnError::Net(m) => f.write_str(m),
        }
    }
}

impl From<CredError> for ConnError {
    fn from(e: CredError) -> Self {
        match e {
            CredError::NeedsLogin(m) => ConnError::NeedsLogin(m),
            CredError::Other(m) => ConnError::Net(m),
        }
    }
}

pub fn tls() -> &'static RustlsConnector {
    static C: OnceLock<RustlsConnector> = OnceLock::new();
    C.get_or_init(|| RustlsConnector::new_with_native_certs().unwrap_or_else(|_| RustlsConnector::new_with_webpki_roots_certs()))
}

fn tcp(host: &str, port: u16) -> Result<TcpStream, ConnError> {
    let addrs = (host, port).to_socket_addrs().map_err(|e| ConnError::Net(format!("{host}: {e}")))?;
    let mut last = None;
    for addr in addrs {
        match TcpStream::connect_timeout(&addr, Duration::from_secs(20)) {
            Ok(s) => {
                s.set_read_timeout(Some(Duration::from_secs(180))).ok();
                s.set_write_timeout(Some(Duration::from_secs(60))).ok();
                return Ok(s);
            }
            Err(e) => last = Some(e),
        }
    }
    Err(ConnError::Net(format!("{host}: {}", last.map(|e| e.to_string()).unwrap_or_else(|| "no address".into()))))
}

struct XOAuth2(String);

impl imap::Authenticator for XOAuth2 {
    type Response = String;
    fn process(&self, challenge: &[u8]) -> String {
        // A non-empty challenge carries an error; an empty reply ends the exchange.
        if challenge.is_empty() {
            self.0.clone()
        } else {
            String::new()
        }
    }
}

pub fn connect(acct: &Account, creds: &Creds) -> Result<Imap, ConnError> {
    let s = &acct.imap;
    let net = |e: imap::Error| ConnError::Net(format!("{}: {e}", s.host));
    let client: imap::Client<Connection> = match s.security {
        Security::Tls => {
            let stream = tls().connect(&s.host, tcp(&s.host, s.port)?).map_err(|e| ConnError::Net(format!("{}: TLS: {e}", s.host)))?;
            let mut c = imap::Client::new(Box::new(stream) as Connection);
            c.read_greeting().map_err(net)?;
            c
        }
        Security::StartTls => imap::ClientBuilder::new(s.host.as_str(), s.port).mode(imap::ConnectionMode::StartTls).connect().map_err(net)?,
        Security::Plain => {
            let mut c = imap::Client::new(Box::new(tcp(&s.host, s.port)?) as Connection);
            c.read_greeting().map_err(net)?;
            c
        }
    };
    let result = match creds.imap(acct)? {
        Login::Password { user, password } => client.login(user, password),
        Login::XOAuth2 { user, token } => client.authenticate("XOAUTH2", &XOAuth2(crate::oauth::xoauth2(&user, &token))),
    };
    match result {
        Ok(session) => Ok(session),
        Err((e @ (imap::Error::No(_) | imap::Error::Bad(_)), _)) => {
            creds.invalidate(acct);
            Err(ConnError::Auth(format!("{} rejected the login: {e}", s.host)))
        }
        Err((e, _)) => Err(net(e)),
    }
}
