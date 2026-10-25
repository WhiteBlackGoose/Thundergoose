//! Hands out passwords and fresh OAuth access tokens to the IMAP/SMTP threads.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::config::{save_json, Account, Auth, Issuer, Paths, Secrets};
use crate::oauth;

#[derive(Debug, Clone)]
pub enum CredError {
    /// The user has to sign in (again) or enter a password.
    NeedsLogin(String),
    Other(String),
}

impl std::fmt::Display for CredError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CredError::NeedsLogin(m) | CredError::Other(m) => f.write_str(m),
        }
    }
}

pub enum Login {
    Password { user: String, password: String },
    XOAuth2 { user: String, token: String },
}

pub struct Creds {
    secrets_path: PathBuf,
    secrets: Mutex<Secrets>,
    tokens: Mutex<HashMap<String, (String, Instant)>>,
    /// The linked Thunderbird profile, if the user chose to stay linked.
    tb_profile: Mutex<Option<PathBuf>>,
}

impl Creds {
    pub fn new(paths: &Paths, secrets: Secrets, tb_profile: Option<PathBuf>) -> Self {
        Creds { secrets_path: paths.secrets(), secrets: Mutex::new(secrets), tokens: Mutex::new(HashMap::new()), tb_profile: Mutex::new(tb_profile) }
    }

    fn save(&self, secrets: &Secrets) {
        if let Err(e) = save_json(&self.secrets_path, secrets, true) {
            eprintln!("cannot save secrets: {e}");
        }
    }

    pub fn set(&self, kind: &str, account: &str, value: String) {
        let mut s = self.secrets.lock().unwrap();
        s.set(kind, account, value);
        self.save(&s);
    }

    pub fn forget(&self, account: &str) {
        let mut s = self.secrets.lock().unwrap();
        s.0.retain(|k, _| k.split_once(':').map(|(_, a)| a) != Some(account));
        self.save(&s);
        self.tokens.lock().unwrap().remove(account);
    }

    pub fn set_tb_profile(&self, profile: Option<PathBuf>) {
        *self.tb_profile.lock().unwrap() = profile;
    }

    pub fn invalidate(&self, acct: &Account) {
        self.tokens.lock().unwrap().remove(&acct.id);
    }

    /// Picks up a token Thunderbird may have refreshed since we imported it.
    fn token_from_thunderbird(&self, acct: &Account, issuer: Issuer) -> Option<String> {
        let profile = self.tb_profile.lock().unwrap().clone()?;
        let origin = match issuer {
            Issuer::Google => "oauth://accounts.google.com",
            Issuer::Microsoft => "oauth://login.microsoftonline.com",
        };
        crate::tb::read_logins(&profile, "")
            .ok()?
            .into_iter()
            .find(|l| l.origin == origin && (l.username.eq_ignore_ascii_case(&acct.imap.username) || l.username.eq_ignore_ascii_case(&acct.email)))
            .map(|l| l.password)
    }

    pub fn access_token(&self, acct: &Account) -> Result<String, CredError> {
        let Auth::OAuth2(issuer) = acct.auth else { return Err(CredError::Other("not an OAuth account".into())) };
        if let Some((tok, until)) = self.tokens.lock().unwrap().get(&acct.id) {
            if Instant::now() < *until {
                return Ok(tok.clone());
            }
        }
        let refresh = self.secrets.lock().unwrap().get("refresh", &acct.id);
        let Some(refresh) = refresh else { return Err(CredError::NeedsLogin("Sign in required".into())) };
        let result = match oauth::refresh(issuer, &refresh) {
            Err(e) if e.needs_login => match self.token_from_thunderbird(acct, issuer).filter(|t| *t != refresh) {
                Some(tb_token) => {
                    let r = oauth::refresh(issuer, &tb_token);
                    if r.is_ok() {
                        self.set("refresh", &acct.id, tb_token);
                    }
                    r
                }
                None => Err(e),
            },
            r => r,
        };
        match result {
            Ok(t) => {
                if let Some(new_refresh) = t.refresh {
                    self.set("refresh", &acct.id, new_refresh);
                }
                let until = Instant::now() + Duration::from_secs(t.expires_in.saturating_sub(120).max(60));
                self.tokens.lock().unwrap().insert(acct.id.clone(), (t.access.clone(), until));
                Ok(t.access)
            }
            Err(e) if e.needs_login => Err(CredError::NeedsLogin(format!("Sign in again ({e})"))),
            Err(e) => Err(CredError::Other(e.message)),
        }
    }

    fn login_for(&self, acct: &Account, kind: &str, user: &str) -> Result<Login, CredError> {
        match acct.auth {
            Auth::OAuth2(_) => Ok(Login::XOAuth2 { user: user.to_string(), token: self.access_token(acct)? }),
            Auth::Password => {
                let s = self.secrets.lock().unwrap();
                // SMTP usually shares the IMAP password.
                let pw = s.get(kind, &acct.id).or_else(|| s.get("imap", &acct.id));
                match pw {
                    Some(password) => Ok(Login::Password { user: user.to_string(), password }),
                    None => Err(CredError::NeedsLogin("Password required".into())),
                }
            }
        }
    }

    pub fn imap(&self, acct: &Account) -> Result<Login, CredError> {
        self.login_for(acct, "imap", &acct.imap.username)
    }

    pub fn smtp(&self, acct: &Account) -> Result<Login, CredError> {
        self.login_for(acct, "smtp", &acct.smtp.username)
    }

    /// Interactive browser sign-in (blocking; run off the UI thread).
    pub fn sign_in(&self, acct: &Account) -> Result<(), String> {
        let Auth::OAuth2(issuer) = acct.auth else { return Err("not an OAuth account".into()) };
        let t = oauth::interactive_login(issuer, &acct.email).map_err(|e| e.message)?;
        let refresh = t.refresh.ok_or("the provider returned no refresh token")?;
        self.set("refresh", &acct.id, refresh);
        let until = Instant::now() + Duration::from_secs(t.expires_in.saturating_sub(120).max(60));
        self.tokens.lock().unwrap().insert(acct.id.clone(), (t.access, until));
        Ok(())
    }
}
