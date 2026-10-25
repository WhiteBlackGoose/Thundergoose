//! Imports accounts, credentials and contact names from a Thunderbird profile.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::config::{issuer_for_host, Account, Auth, Issuer, Secrets, Security, Server};
use crate::nss::{KeyStore, NssError};

/// Opens a copy of an SQLite file (with its WAL) so Thunderbird's locks never get in the way.
pub fn open_sqlite_copy(path: &Path) -> rusqlite::Result<rusqlite::Connection> {
    let dir = std::env::temp_dir().join(format!("thundergoose-{}", std::process::id()));
    std::fs::create_dir_all(&dir).ok();
    let name = path.file_name().unwrap_or_default();
    let dst = dir.join(name);
    std::fs::copy(path, &dst).map_err(|_| rusqlite::Error::InvalidPath(path.to_path_buf()))?;
    for ext in ["-wal", "-shm"] {
        let side = PathBuf::from(format!("{}{ext}", path.display()));
        let side_dst = PathBuf::from(format!("{}{ext}", dst.display()));
        if side.exists() {
            std::fs::copy(side, side_dst).ok();
        } else {
            std::fs::remove_file(side_dst).ok();
        }
    }
    rusqlite::Connection::open(dst)
}

fn roots() -> Vec<PathBuf> {
    let mut v = Vec::new();
    let home = crate::platform::home_dir().unwrap_or_default();
    if cfg!(target_os = "windows") {
        if let Some(app) = std::env::var_os("APPDATA") {
            v.push(PathBuf::from(app).join("Thunderbird"));
        }
    } else if cfg!(target_os = "macos") {
        v.push(home.join("Library/Thunderbird"));
    } else {
        v.push(home.join(".thunderbird"));
        v.push(home.join(".var/app/org.mozilla.Thunderbird/.thunderbird"));
        v.push(home.join("snap/thunderbird/common/.thunderbird"));
    }
    v
}

/// The default profile directory of the local Thunderbird installation.
pub fn find_profile() -> Option<PathBuf> {
    for root in roots() {
        let Ok(ini) = std::fs::read_to_string(root.join("profiles.ini")) else { continue };
        let mut sections: Vec<HashMap<String, String>> = Vec::new();
        let mut names = Vec::new();
        for line in ini.lines().map(str::trim) {
            if line.starts_with('[') {
                names.push(line.trim_matches(['[', ']']).to_string());
                sections.push(HashMap::new());
            } else if let (Some((k, v)), Some(s)) = (line.split_once('='), sections.last_mut()) {
                s.insert(k.to_string(), v.to_string());
            }
        }
        let resolve = |s: &HashMap<String, String>, key: &str| {
            let p = s.get(key)?;
            let rel = s.get("IsRelative").map(|v| v == "1").unwrap_or(true);
            Some(if rel { root.join(p) } else { PathBuf::from(p) })
        };
        // An [Install...] section's Default= wins, then the profile marked Default=1, then any profile.
        let install = names.iter().zip(&sections).find(|(n, _)| n.starts_with("Install")).and_then(|(_, s)| s.get("Default"));
        if let Some(p) = install {
            let p = root.join(p);
            if p.join("prefs.js").exists() {
                return Some(p);
            }
        }
        let profiles = || names.iter().zip(&sections).filter(|(n, _)| n.starts_with("Profile")).map(|(_, s)| s);
        if let Some(p) = profiles().find(|s| s.get("Default").map(String::as_str) == Some("1")).and_then(|s| resolve(s, "Path")) {
            return Some(p);
        }
        if let Some(p) = profiles().find_map(|s| resolve(s, "Path")) {
            return Some(p);
        }
    }
    None
}

#[derive(Debug, Clone)]
enum Pref {
    Str(String),
    Int(i64),
}

fn parse_prefs(src: &str) -> HashMap<String, Pref> {
    fn string(s: &str) -> Option<(String, &str)> {
        let s = s.trim_start().strip_prefix('"')?;
        let mut out = String::new();
        let mut chars = s.char_indices();
        while let Some((i, c)) = chars.next() {
            match c {
                '\\' => {
                    let (_, e) = chars.next()?;
                    match e {
                        'n' => out.push('\n'),
                        'u' => {
                            let hex: String = (0..4).filter_map(|_| chars.next().map(|(_, c)| c)).collect();
                            out.extend(u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32));
                        }
                        other => out.push(other),
                    }
                }
                '"' => return Some((out, &s[i + 1..])),
                c => out.push(c),
            }
        }
        None
    }
    let mut prefs = HashMap::new();
    for line in src.lines() {
        let Some(rest) = line.trim().strip_prefix("user_pref(") else { continue };
        let Some((key, rest)) = string(rest) else { continue };
        let rest = rest.trim_start().trim_start_matches(',').trim_start();
        let value = if rest.starts_with('"') {
            string(rest).map(|(s, _)| Pref::Str(s))
        } else {
            let tok = rest.split(')').next().unwrap_or("").trim();
            match tok {
                "true" => Some(Pref::Int(1)),
                "false" => Some(Pref::Int(0)),
                t => t.parse().ok().map(Pref::Int),
            }
        };
        if let Some(v) = value {
            prefs.insert(key, v);
        }
    }
    prefs
}

struct Prefs(HashMap<String, Pref>);

impl Prefs {
    fn str(&self, key: &str) -> Option<String> {
        match self.0.get(key)? {
            Pref::Str(s) => Some(s.clone()),
            _ => None,
        }
    }
    fn int(&self, key: &str) -> Option<i64> {
        match self.0.get(key)? {
            Pref::Int(i) => Some(*i),
            Pref::Str(s) => s.parse().ok(),
        }
    }
}

fn security(socket_type: Option<i64>, port: u16, implicit_port: u16) -> Security {
    match socket_type {
        Some(3) => Security::Tls,
        Some(2) => Security::StartTls,
        Some(0) if port != implicit_port => Security::Plain,
        _ if port == implicit_port => Security::Tls,
        _ => Security::StartTls,
    }
}

pub struct Login {
    pub origin: String,
    pub realm: Option<String>,
    pub username: String,
    pub password: String,
}

pub fn read_logins(profile: &Path, primary_password: &str) -> Result<Vec<Login>, NssError> {
    let keys = KeyStore::open(profile, primary_password)?;
    let json: serde_json::Value = match std::fs::read(profile.join("logins.json")) {
        Ok(b) => serde_json::from_slice(&b).map_err(|e| NssError::Corrupt(e.to_string()))?,
        Err(_) => return Ok(Vec::new()),
    };
    let mut out = Vec::new();
    for l in json["logins"].as_array().into_iter().flatten() {
        let (Some(origin), Some(u), Some(p)) = (l["hostname"].as_str(), l["encryptedUsername"].as_str(), l["encryptedPassword"].as_str()) else {
            continue;
        };
        let (Some(username), Some(password)) = (keys.decrypt(u), keys.decrypt(p)) else { continue };
        out.push(Login { origin: origin.to_string(), realm: l["httpRealm"].as_str().map(str::to_string), username, password });
    }
    Ok(out)
}

pub struct Import {
    pub accounts: Vec<Account>,
    pub secrets: Secrets,
    pub warnings: Vec<String>,
}

/// Reads every IMAP account of the profile, with its passwords / OAuth refresh tokens.
pub fn import(profile: &Path, primary_password: &str) -> Result<Import, String> {
    let src = std::fs::read_to_string(profile.join("prefs.js")).map_err(|e| format!("cannot read prefs.js: {e}"))?;
    let p = Prefs(parse_prefs(&src));
    let mut warnings = Vec::new();
    let logins = match read_logins(profile, primary_password) {
        Ok(l) => l,
        Err(e @ NssError::PrimaryPassword) => return Err(e.to_string()),
        Err(e) => {
            warnings.push(format!("Saved passwords unavailable: {e}"));
            Vec::new()
        }
    };
    let find_password = |scheme: &str, host: &str, user: &str| {
        let origin = format!("{scheme}://{host}");
        let mut it = logins.iter().filter(|l| l.origin.eq_ignore_ascii_case(&origin));
        let all: Vec<_> = it.by_ref().collect();
        all.iter().find(|l| l.username.eq_ignore_ascii_case(user)).or(all.first()).map(|l| l.password.clone())
    };
    let find_token = |issuer: Issuer, user: &str| {
        let origin = match issuer {
            Issuer::Google => "oauth://accounts.google.com",
            Issuer::Microsoft => "oauth://login.microsoftonline.com",
        };
        logins
            .iter()
            .filter(|l| l.origin == origin && l.username.eq_ignore_ascii_case(user))
            .max_by_key(|l| l.realm.as_deref().map(|r| r.contains("mail") || r.contains("IMAP")).unwrap_or(false))
            .map(|l| l.password.clone())
    };

    let order = p.str("mail.accountmanager.accounts").unwrap_or_default();
    let mut accounts = Vec::new();
    let mut secrets = Secrets::default();
    for acct in order.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let Some(server) = p.str(&format!("mail.account.{acct}.server")) else { continue };
        let sp = |k: &str| format!("mail.server.{server}.{k}");
        if p.str(&sp("type")).as_deref() != Some("imap") {
            continue;
        }
        let host = p.str(&sp("realhostname")).or_else(|| p.str(&sp("hostname"))).unwrap_or_default();
        let user = p.str(&sp("realuserName")).or_else(|| p.str(&sp("userName"))).unwrap_or_default();
        let port = p.int(&sp("port")).unwrap_or(993) as u16;
        let imap = Server { security: security(p.int(&sp("socketType")), port, 993), host, port, username: user.clone() };

        let ids: Vec<String> = p.str(&format!("mail.account.{acct}.identities")).unwrap_or_default().split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
        let Some(first) = ids.first() else {
            warnings.push(format!("{}: no identity, skipped", imap.username));
            continue;
        };
        let email = p.str(&format!("mail.identity.{first}.useremail")).unwrap_or_else(|| user.clone());
        let name = p.str(&format!("mail.identity.{first}.fullName")).unwrap_or_default();
        let aliases = ids[1..].iter().filter_map(|i| p.str(&format!("mail.identity.{i}.useremail"))).filter(|a| !a.eq_ignore_ascii_case(&email)).collect();

        let smtp_key = p.str(&format!("mail.identity.{first}.smtpServer")).or_else(|| p.str("mail.smtp.defaultserver"));
        let Some(smtp_key) = smtp_key else {
            warnings.push(format!("{email}: no outgoing server, skipped"));
            continue;
        };
        let mp = |k: &str| format!("mail.smtpserver.{smtp_key}.{k}");
        let smtp_port = p.int(&mp("port")).unwrap_or(587) as u16;
        let smtp = Server {
            host: p.str(&mp("hostname")).unwrap_or_default(),
            port: smtp_port,
            security: security(p.int(&mp("try_ssl")).or(p.int(&mp("socketType"))), smtp_port, 465),
            username: p.str(&mp("username")).unwrap_or_else(|| user.clone()),
        };

        let oauth = p.int(&sp("authMethod")) == Some(10);
        let auth = match (oauth, issuer_for_host(&imap.host)) {
            (true, Some(issuer)) => Auth::OAuth2(issuer),
            (true, None) => {
                warnings.push(format!("{email}: OAuth provider of {} is not supported, skipped", imap.host));
                continue;
            }
            _ => Auth::Password,
        };
        match auth {
            Auth::OAuth2(issuer) => match find_token(issuer, &user).or_else(|| find_token(issuer, &email)) {
                Some(token) => secrets.set("refresh", &email, token),
                None => warnings.push(format!("{email}: no OAuth token in Thunderbird, sign in again")),
            },
            Auth::Password => {
                match find_password("imap", &imap.host, &imap.username) {
                    Some(pw) => secrets.set("imap", &email, pw),
                    None => warnings.push(format!("{email}: no saved IMAP password")),
                }
                match find_password("smtp", &smtp.host, &smtp.username) {
                    Some(pw) => secrets.set("smtp", &email, pw),
                    None => warnings.push(format!("{email}: no saved SMTP password")),
                }
            }
        }
        let save_sent = issuer_for_host(&imap.host).is_none();
        accounts.push(Account { id: email.clone(), email, name, aliases, imap, smtp, auth, save_sent, tag: None, tag_color: None });
    }
    Ok(Import { accounts, secrets, warnings })
}

/// Display names of everyone in the Thunderbird address books, keyed by lowercase address.
pub fn address_book(profile: &Path) -> HashMap<String, String> {
    let mut names = HashMap::new();
    let Ok(dir) = std::fs::read_dir(profile) else { return names };
    let mut books: Vec<PathBuf> = dir
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            let n = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            (n.starts_with("abook") || n == "history.sqlite") && n.ends_with(".sqlite")
        })
        .collect();
    // Collected addresses first so real address book entries overwrite them.
    books.sort_by_key(|p| !p.ends_with("history.sqlite"));
    for book in books {
        let Ok(conn) = open_sqlite_copy(&book) else { continue };
        let Ok(mut stmt) = conn.prepare("SELECT card, name, value FROM properties") else { continue };
        let mut cards: HashMap<String, HashMap<String, String>> = HashMap::new();
        if let Ok(rows) = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))) {
            for (card, k, v) in rows.flatten() {
                cards.entry(card).or_default().insert(k, v);
            }
        }
        for props in cards.values() {
            let display = props
                .get("DisplayName")
                .filter(|s| !s.trim().is_empty())
                .cloned()
                .or_else(|| {
                    let n = format!("{} {}", props.get("FirstName").map_or("", |s| s), props.get("LastName").map_or("", |s| s));
                    (!n.trim().is_empty()).then(|| n.trim().to_string())
                });
            let Some(display) = display else { continue };
            for key in ["PrimaryEmail", "SecondEmail"] {
                if let Some(addr) = props.get(key).filter(|a| a.contains('@')) {
                    names.insert(addr.to_lowercase(), display.clone());
                }
            }
        }
    }
    names
}
