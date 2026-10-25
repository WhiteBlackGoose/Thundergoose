//! Accounts, secrets and app settings, persisted as JSON in the data directory.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Security {
    Tls,
    StartTls,
    Plain,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Server {
    pub host: String,
    pub port: u16,
    pub security: Security,
    pub username: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Issuer {
    Google,
    Microsoft,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Auth {
    Password,
    OAuth2(Issuer),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Account {
    /// Stable id; the primary address.
    pub id: String,
    pub email: String,
    pub name: String,
    /// Further addresses that count as "me" for this account (other identities).
    #[serde(default)]
    pub aliases: Vec<String>,
    pub imap: Server,
    pub smtp: Server,
    pub auth: Auth,
    /// Upload sent mail to the Sent folder ourselves (Gmail and Office 365 do it server-side).
    pub save_sent: bool,
    /// The user's name for the account's tags and chips; the domain when unset.
    #[serde(default)]
    pub tag: Option<String>,
    /// Index into the tag palette; derived from the address when unset.
    #[serde(default)]
    pub tag_color: Option<usize>,
}

impl Account {
    pub fn short_label(&self) -> String {
        if let Some(tag) = self.tag.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
            return tag.to_string();
        }
        self.default_label()
    }

    pub fn default_label(&self) -> String {
        let domain = self.email.rsplit('@').next().unwrap_or(&self.email);
        domain.split('.').next().unwrap_or(domain).to_string()
    }

    pub fn addresses(&self) -> impl Iterator<Item = &String> {
        std::iter::once(&self.email).chain(self.aliases.iter())
    }
}

pub fn issuer_for_host(host: &str) -> Option<Issuer> {
    let h = host.to_ascii_lowercase();
    if h.ends_with("gmail.com") || h.ends_with("googlemail.com") || h.ends_with("google.com") {
        Some(Issuer::Google)
    } else if h.ends_with("office365.com") || h.ends_with("outlook.com") || h.ends_with("hotmail.com") {
        Some(Issuer::Microsoft)
    } else {
        None
    }
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Settings {
    /// 0 violet, 1 blue, 2 teal, 3 orange, 4 rose
    #[serde(default)]
    pub accent: i32,
    /// 0 system, 1 light, 2 dark
    #[serde(default)]
    pub theme: i32,
    #[serde(default)]
    pub thunderbird_profile: Option<PathBuf>,
    #[serde(default = "yes")]
    pub notify_desktop: bool,
    #[serde(default = "yes")]
    pub notify_sound: bool,
    #[serde(default = "yes")]
    pub favicons: bool,
    /// Keep reading contact names and renewed sign-ins from the Thunderbird profile. Unset
    /// means: linked if accounts were imported from it.
    #[serde(default)]
    pub thunderbird_linked: Option<bool>,
    /// Interface size on top of the system's scaling (1.0 = as the system says).
    #[serde(default = "one")]
    pub ui_scale: f32,
    /// Width of the conversation list, in logical pixels.
    #[serde(default = "sidebar_width")]
    pub sidebar_width: f32,
}

fn sidebar_width() -> f32 {
    360.0
}

fn one() -> f32 {
    1.0
}

impl Settings {
    pub fn tb_linked(&self) -> bool {
        self.thunderbird_linked.unwrap_or(self.thunderbird_profile.is_some())
    }
}

impl Default for Settings {
    fn default() -> Self {
        Settings { accent: 0, theme: 0, thunderbird_profile: None, notify_desktop: true, notify_sound: true, favicons: true, thunderbird_linked: None, ui_scale: 1.0, sidebar_width: 360.0 }
    }
}

pub struct Paths {
    pub data: PathBuf,
}

impl Paths {
    pub fn new() -> Self {
        let data = std::env::var_os("THUNDERGOOSE_HOME")
            .map(PathBuf::from)
            .or_else(crate::platform::data_dir)
            .unwrap_or_else(|| PathBuf::from(".thundergoose"));
        std::fs::create_dir_all(&data).ok();
        Paths { data }
    }
    pub fn accounts(&self) -> PathBuf {
        self.data.join("accounts.json")
    }
    pub fn secrets(&self) -> PathBuf {
        self.data.join("secrets.json")
    }
    pub fn settings(&self) -> PathBuf {
        self.data.join("settings.json")
    }
    pub fn db(&self) -> PathBuf {
        self.data.join("mail.sqlite")
    }
}

pub fn load_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

pub fn save_json<T: Serialize>(path: &Path, value: &T, private: bool) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        if private {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let _ = private;
        let mut f = opts.open(&tmp)?;
        std::io::Write::write_all(&mut f, &serde_json::to_vec_pretty(value)?)?;
    }
    std::fs::rename(tmp, path)
}

/// Passwords and refresh tokens keyed by `<kind>:<account id>` (kinds: imap, smtp, refresh).
#[derive(Default, Serialize, Deserialize)]
pub struct Secrets(pub HashMap<String, String>);

impl Secrets {
    pub fn get(&self, kind: &str, account: &str) -> Option<String> {
        self.0.get(&format!("{kind}:{account}")).cloned()
    }
    pub fn set(&mut self, kind: &str, account: &str, value: String) {
        self.0.insert(format!("{kind}:{account}"), value);
    }
}
