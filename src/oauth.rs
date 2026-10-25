//! OAuth2 for Google and Microsoft, registered exactly like Thunderbird's own client so that the
//! refresh tokens imported from a Thunderbird profile keep working.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::time::Duration;

use base64::Engine as _;
use sha2::{Digest, Sha256};

use crate::config::Issuer;

struct Provider {
    client_id: &'static str,
    client_secret: Option<&'static str>,
    auth_url: &'static str,
    token_url: &'static str,
    scope: &'static str,
    pkce: bool,
}

// Values from Thunderbird's OAuth2Providers.sys.mjs.
fn provider(issuer: Issuer) -> Provider {
    match issuer {
        Issuer::Google => Provider {
            client_id: "406964657835-aq8lmia8j95dhl1a2bvharmfk3t1hgqj.apps.googleusercontent.com",
            client_secret: Some("kSmqreRr0qwBWJgbf5Y-PjSU"),
            auth_url: "https://accounts.google.com/o/oauth2/auth",
            token_url: "https://www.googleapis.com/oauth2/v3/token",
            scope: "https://mail.google.com/",
            pkce: true,
        },
        Issuer::Microsoft => Provider {
            client_id: "9e5f94bc-e8a4-4e73-b8be-63364c29d753",
            client_secret: None,
            auth_url: "https://login.microsoftonline.com/common/oauth2/v2.0/authorize",
            token_url: "https://login.microsoftonline.com/common/oauth2/v2.0/token",
            scope: "https://outlook.office.com/IMAP.AccessAsUser.All https://outlook.office.com/SMTP.Send offline_access",
            pkce: false,
        },
    }
}

pub struct Tokens {
    pub access: String,
    pub expires_in: u64,
    pub refresh: Option<String>,
}

#[derive(Debug)]
pub struct OAuthError {
    /// The refresh token was revoked or expired; only an interactive login helps.
    pub needs_login: bool,
    pub message: String,
}

impl std::fmt::Display for OAuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

fn err(needs_login: bool, message: impl Into<String>) -> OAuthError {
    OAuthError { needs_login, message: message.into() }
}

fn token_request(p: &Provider, params: &[(&str, &str)]) -> Result<Tokens, OAuthError> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(30)))
        .build()
        .into();
    let mut form: Vec<(&str, &str)> = vec![("client_id", p.client_id)];
    if let Some(secret) = p.client_secret {
        form.push(("client_secret", secret));
    }
    form.extend_from_slice(params);
    let mut resp = agent.post(p.token_url).send_form(form).map_err(|e| err(false, format!("token endpoint: {e}")))?;
    let json: serde_json::Value = resp.body_mut().read_json().map_err(|e| err(false, format!("token endpoint: {e}")))?;
    if let Some(e) = json["error"].as_str() {
        let desc = json["error_description"].as_str().unwrap_or("");
        let needs_login = matches!(e, "invalid_grant" | "unauthorized_client" | "invalid_client" | "interaction_required");
        return Err(err(needs_login, format!("{e}: {}", desc.lines().next().unwrap_or(""))));
    }
    let access = json["access_token"].as_str().ok_or_else(|| err(false, "no access_token in response"))?;
    Ok(Tokens {
        access: access.to_string(),
        expires_in: json["expires_in"].as_u64().unwrap_or(3600),
        refresh: json["refresh_token"].as_str().map(str::to_string),
    })
}

/// Microsoft wants a scope on refresh; asking for the mail subset works for tokens with wider grants too.
pub fn refresh(issuer: Issuer, refresh_token: &str) -> Result<Tokens, OAuthError> {
    let p = provider(issuer);
    let mut params = vec![("grant_type", "refresh_token"), ("refresh_token", refresh_token)];
    if issuer == Issuer::Microsoft {
        params.push(("scope", p.scope));
    }
    token_request(&p, &params)
}

fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn random(n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    getrandom::fill(&mut buf).expect("system randomness");
    buf
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                match u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("zz"), 16) {
                    Ok(b) => {
                        out.push(b);
                        i += 3;
                        continue;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            b'+' => out.push(b' '),
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn percent_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Runs the authorization-code flow in the system browser with a loopback redirect, like Thunderbird.
pub fn interactive_login(issuer: Issuer, login_hint: &str) -> Result<Tokens, OAuthError> {
    let p = provider(issuer);
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| err(false, format!("cannot listen on localhost: {e}")))?;
    let port = listener.local_addr().map_err(|e| err(false, e.to_string()))?.port();
    let redirect = format!("http://localhost:{port}");
    let state = b64url(&random(16));
    let verifier = b64url(&random(48));
    let challenge = b64url(&Sha256::digest(verifier.as_bytes()));

    let mut url = format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&scope={}&login_hint={}&state={}",
        p.auth_url,
        percent_encode(p.client_id),
        percent_encode(&redirect),
        percent_encode(p.scope),
        percent_encode(login_hint),
        state
    );
    if p.pkce {
        url.push_str(&format!("&code_challenge={challenge}&code_challenge_method=S256"));
    }

    // Browsers may resolve localhost to ::1, so listen there too.
    let (tx, rx) = mpsc::channel::<Option<(String, String)>>();
    let serve = |listener: TcpListener, tx: mpsc::Sender<Option<(String, String)>>| {
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut line = String::new();
                if BufReader::new(&stream).read_line(&mut line).is_err() {
                    continue;
                }
                let target = line.split_whitespace().nth(1).unwrap_or("/");
                let query = target.split_once('?').map(|(_, q)| q).unwrap_or("");
                let mut code = None;
                let mut st = String::new();
                let mut error = None;
                for kv in query.split('&') {
                    let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
                    match k {
                        "code" => code = Some(percent_decode(v)),
                        "state" => st = percent_decode(v),
                        "error" => error = Some(percent_decode(v)),
                        _ => {}
                    }
                }
                if code.is_none() && error.is_none() {
                    let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
                    continue;
                }
                let body = if code.is_some() {
                    "<h2 style='font-family:sans-serif'>Signed in. You can close this tab and return to Thundergoose.</h2>"
                } else {
                    "<h2 style='font-family:sans-serif'>Sign-in failed. Return to Thundergoose and try again.</h2>"
                };
                let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                let _ = tx.send(code.map(|c| (c, st)));
                break;
            }
        });
    };
    if let Ok(v6) = TcpListener::bind(("::1", port)) {
        serve(v6, tx.clone());
    }
    serve(listener, tx);

    open::that(&url).map_err(|e| err(false, format!("cannot open browser: {e}")))?;
    let (code, got_state) = match rx.recv_timeout(Duration::from_secs(300)) {
        Ok(Some(v)) => v,
        Ok(None) => return Err(err(true, "sign-in was cancelled or denied")),
        Err(_) => return Err(err(true, "sign-in timed out")),
    };
    if got_state != state {
        return Err(err(true, "sign-in response did not match the request"));
    }
    let mut params = vec![("grant_type", "authorization_code"), ("code", code.as_str()), ("redirect_uri", redirect.as_str())];
    if p.pkce {
        params.push(("code_verifier", verifier.as_str()));
    }
    if issuer == Issuer::Microsoft {
        params.push(("scope", p.scope));
    }
    token_request(&p, &params)
}

/// SASL XOAUTH2 initial response (before base64).
pub fn xoauth2(user: &str, token: &str) -> String {
    format!("user={user}\x01auth=Bearer {token}\x01\x01")
}
