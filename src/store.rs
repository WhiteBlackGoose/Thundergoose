//! Local mail cache. A mail is stored once per Message-ID no matter how many folders or accounts
//! it shows up in; `locs` remembers where it lives on the servers.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

pub const SCHEMA: &str = "
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
CREATE TABLE IF NOT EXISTS mails(
    id INTEGER PRIMARY KEY,
    msgid TEXT NOT NULL UNIQUE,
    date INTEGER NOT NULL,
    from_addr TEXT NOT NULL,
    from_name TEXT NOT NULL,
    recipients TEXT NOT NULL,
    subject TEXT NOT NULL,
    peer TEXT NOT NULL,
    outgoing INTEGER NOT NULL,
    seen INTEGER NOT NULL,
    in_reply_to TEXT NOT NULL DEFAULT '',
    refs TEXT NOT NULL DEFAULT '',
    account TEXT NOT NULL,
    me_addr TEXT NOT NULL,
    size INTEGER NOT NULL DEFAULT 0,
    has_body INTEGER NOT NULL DEFAULT 0,
    text TEXT NOT NULL DEFAULT '',
    quoted INTEGER NOT NULL DEFAULT 0,
    has_html INTEGER NOT NULL DEFAULT 0,
    attachments TEXT NOT NULL DEFAULT '[]',
    snippet TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS mails_peer ON mails(peer, date);
CREATE INDEX IF NOT EXISTS mails_date ON mails(date);
CREATE INDEX IF NOT EXISTS mails_body ON mails(account, has_body, date);
CREATE TABLE IF NOT EXISTS raw(mail INTEGER PRIMARY KEY, data BLOB NOT NULL);
CREATE TABLE IF NOT EXISTS locs(
    account TEXT NOT NULL, folder TEXT NOT NULL, uid INTEGER NOT NULL, mail INTEGER NOT NULL,
    PRIMARY KEY(account, folder, uid)
);
CREATE INDEX IF NOT EXISTS locs_mail ON locs(mail);
CREATE TABLE IF NOT EXISTS folders(
    account TEXT NOT NULL, name TEXT NOT NULL, uidvalidity INTEGER NOT NULL,
    high INTEGER NOT NULL, low INTEGER NOT NULL,
    PRIMARY KEY(account, name)
);
CREATE TABLE IF NOT EXISTS names(addr TEXT PRIMARY KEY, name TEXT NOT NULL, prio INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS junk_folders(account TEXT NOT NULL, name TEXT NOT NULL, PRIMARY KEY(account, name));
CREATE TABLE IF NOT EXISTS contacts(
    key TEXT PRIMARY KEY,
    favorite INTEGER NOT NULL DEFAULT 0,
    muted INTEGER NOT NULL DEFAULT 0,
    ignored INTEGER NOT NULL DEFAULT 0
);
";

/// What the user decided about a conversation.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Flags {
    pub favorite: bool,
    pub muted: bool,
    pub ignored: bool,
    /// 1 marked as spam, -1 marked as not spam, 0 up to the Junk folders.
    pub spam: i32,
}

#[derive(Clone, Copy, Debug)]
pub enum Flag {
    Favorite,
    Muted,
    Ignored,
}

/// Columns added after the first release, as (table, column, declaration).
const ADDED_COLUMNS: &[(&str, &str, &str)] = &[
    ("mails", "junk", "INTEGER NOT NULL DEFAULT 0"),
    ("mails", "rich", "INTEGER NOT NULL DEFAULT 0"),
    ("contacts", "spam", "INTEGER NOT NULL DEFAULT 0"),
    ("contacts", "remote_images", "INTEGER NOT NULL DEFAULT 0"),
];

/// SQL value: whether mail `m` lives only in Junk folders.
const JUNK_EXPR: &str = "(EXISTS(SELECT 1 FROM locs l JOIN junk_folders j ON j.account=l.account AND j.name=l.folder WHERE l.mail=m.id)
    AND NOT EXISTS(SELECT 1 FROM locs l WHERE l.mail=m.id AND NOT EXISTS(SELECT 1 FROM junk_folders j WHERE j.account=l.account AND j.name=l.folder)))";

/// SQL condition selecting the mail shown in a conversation (`?1` is the key). A one-person
/// conversation also shows the group mail that person took part in.
fn conversation_filter(peer: &str) -> &'static str {
    if peer.is_empty() || peer.contains(',') {
        "peer = ?1"
    } else {
        "(peer = ?1 OR instr(',' || peer || ',', ',' || ?1 || ',') > 0)"
    }
}

/// Bump when `parse::body` changes so stored bodies get parsed again.
pub const PARSE_VERSION: i32 = 8;

pub fn open(path: &Path) -> rusqlite::Result<Connection> {
    let c = Connection::open(path)?;
    c.busy_timeout(std::time::Duration::from_secs(10))?;
    c.execute_batch(SCHEMA)?;
    for (table, column, decl) in ADDED_COLUMNS {
        let exists: bool = c.query_row(&format!("SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name=?1"), [column], |r| r.get::<_, i64>(0)).map(|n| n > 0)?;
        if !exists {
            c.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"))?;
        }
    }
    Ok(c)
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Person {
    pub name: String,
    pub addr: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Attachment {
    pub name: String,
    pub mime: String,
    pub size: usize,
    /// Index into mail-parser's attachment list.
    pub index: usize,
}

/// What we know of a mail from its envelope.
pub struct Header {
    pub msgid: String,
    pub date: i64,
    pub from: Person,
    pub to: Vec<Person>,
    pub cc: Vec<Person>,
    pub subject: String,
    pub in_reply_to: String,
    pub size: u32,
    pub seen: bool,
}

/// Classification of a header from the point of view of "me".
pub struct Placement {
    pub peer: String,
    pub outgoing: bool,
    pub me_addr: String,
}

pub fn place(h: &Header, me: &HashSet<String>, account_email: &str) -> Placement {
    let from = h.from.addr.to_lowercase();
    let outgoing = me.contains(&from);
    let mut others: Vec<String> = std::iter::once(&h.from)
        .chain(&h.to)
        .chain(&h.cc)
        .map(|p| p.addr.to_lowercase())
        .filter(|a| !a.is_empty() && !me.contains(a))
        .collect();
    others.sort();
    others.dedup();
    let me_addr = if outgoing {
        from
    } else {
        h.to.iter().chain(&h.cc).map(|p| p.addr.to_lowercase()).find(|a| me.contains(a)).unwrap_or_else(|| account_email.to_lowercase())
    };
    Placement { peer: others.join(","), outgoing, me_addr }
}

pub struct Body {
    /// The HTML version has layout worth showing (see `html::is_rich`).
    pub rich: bool,
    pub text: String,
    pub quoted: bool,
    pub has_html: bool,
    pub attachments: Vec<Attachment>,
    pub refs: String,
    pub snippet: String,
}

pub struct Store {
    pub conn: Connection,
}

#[derive(Clone, Debug)]
pub struct PeerRow {
    pub peer: String,
    pub last_date: i64,
    pub snippet: String,
    pub outgoing: bool,
    pub unread: i64,
    /// All of the conversation's mail is in Junk folders.
    pub junk: bool,
    /// Own addresses the conversation uses, most recent first.
    pub me_addrs: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct MailRow {
    pub id: i64,
    pub msgid: String,
    pub date: i64,
    pub from: Person,
    pub subject: String,
    pub outgoing: bool,
    pub seen: bool,
    pub account: String,
    pub me_addr: String,
    pub has_body: bool,
    pub text: String,
    pub quoted: bool,
    pub has_html: bool,
    pub attachments: Vec<Attachment>,
    pub refs: String,
    pub in_reply_to: String,
    /// The conversation the mail belongs to (differs from the open one for group mail).
    pub peer: String,
    /// To and Cc.
    pub recipients: Vec<Person>,
    /// Only in Junk folders.
    pub junk: bool,
    pub rich: bool,
}

impl Store {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        Ok(Store { conn: open(path)? })
    }

    pub fn folder_state(&self, account: &str, folder: &str) -> Option<(u32, u32, u32)> {
        self.conn
            .query_row("SELECT uidvalidity, high, low FROM folders WHERE account=?1 AND name=?2", params![account, folder], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .optional()
            .ok()
            .flatten()
    }

    pub fn set_folder_state(&self, account: &str, folder: &str, validity: u32, high: u32, low: u32) {
        let _ = self.conn.execute(
            "INSERT INTO folders(account, name, uidvalidity, high, low) VALUES(?1,?2,?3,?4,?5)
             ON CONFLICT(account, name) DO UPDATE SET uidvalidity=?3, high=?4, low=?5",
            params![account, folder, validity, high, low],
        );
    }

    /// Forgets everything known about a folder (its UIDVALIDITY changed).
    pub fn reset_folder(&self, account: &str, folder: &str) {
        let _ = self.conn.execute("DELETE FROM locs WHERE account=?1 AND folder=?2", params![account, folder]);
        let _ = self.conn.execute("DELETE FROM folders WHERE account=?1 AND name=?2", params![account, folder]);
        self.drop_orphans();
    }

    pub fn drop_orphans(&self) {
        let _ = self.conn.execute_batch(
            "DELETE FROM raw WHERE mail IN (SELECT id FROM mails WHERE id NOT IN (SELECT mail FROM locs) AND outgoing = 0);
             DELETE FROM mails WHERE id NOT IN (SELECT mail FROM locs) AND outgoing = 0;",
        );
    }

    /// Inserts a mail (or finds it by Message-ID) and records its location.
    /// Returns the mail id and whether the mail is new to the store.
    pub fn add(&self, account: &str, folder: &str, uid: u32, h: &Header, p: &Placement) -> rusqlite::Result<(i64, bool)> {
        let id: Option<i64> = self.conn.query_row("SELECT id FROM mails WHERE msgid=?1", [&h.msgid], |r| r.get(0)).optional()?;
        let inserted = id.is_none();
        let id = match id {
            Some(id) => {
                if h.seen {
                    self.conn.execute("UPDATE mails SET seen=1 WHERE id=?1", [id])?;
                }
                id
            }
            None => {
                let mut recipients = h.to.clone();
                recipients.extend(h.cc.iter().cloned());
                self.conn.execute(
                    "INSERT INTO mails(msgid, date, from_addr, from_name, recipients, subject, peer, outgoing, seen, in_reply_to, account, me_addr, size, snippet)
                     VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?6)",
                    params![
                        h.msgid,
                        h.date,
                        h.from.addr.to_lowercase(),
                        h.from.name,
                        serde_json::to_string(&recipients).unwrap_or_default(),
                        h.subject,
                        p.peer,
                        p.outgoing,
                        h.seen || p.outgoing,
                        h.in_reply_to,
                        account,
                        p.me_addr,
                        h.size,
                    ],
                )?;
                self.conn.last_insert_rowid()
            }
        };
        self.conn.execute("INSERT OR REPLACE INTO locs(account, folder, uid, mail) VALUES(?1,?2,?3,?4)", params![account, folder, uid, id])?;
        for person in std::iter::once(&h.from).chain(&h.to).chain(&h.cc) {
            if !person.name.is_empty() && person.name != person.addr {
                self.conn.execute(
                    "INSERT INTO names(addr, name, prio) VALUES(?1, ?2, 1) ON CONFLICT(addr) DO UPDATE SET name=?2 WHERE prio <= 1",
                    params![person.addr.to_lowercase(), person.name],
                )?;
            }
        }
        Ok((id, inserted))
    }

    /// Recomputes every mail's conversation after the set of own addresses changed.
    pub fn replace_me(&self, me: &HashSet<String>) -> rusqlite::Result<()> {
        let rows: Vec<(i64, String, String, String, String, String)> = {
            let mut stmt = self.conn.prepare("SELECT id, msgid, from_addr, from_name, recipients, me_addr FROM mails")?;
            let r = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?;
            r.flatten().collect()
        };
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        for (id, msgid, from_addr, from_name, recipients, me_addr) in rows {
            let to: Vec<Person> = serde_json::from_str(&recipients).unwrap_or_default();
            let h = Header { msgid, date: 0, from: Person { name: from_name, addr: from_addr }, to, cc: Vec::new(), subject: String::new(), in_reply_to: String::new(), size: 0, seen: false };
            let p = place(&h, me, &me_addr);
            self.conn.execute("UPDATE mails SET peer=?2, outgoing=?3, me_addr=?4 WHERE id=?1", params![id, p.peer, p.outgoing, p.me_addr])?;
        }
        self.conn.execute_batch("COMMIT")
    }

    /// Forgets an account's server locations and the mail only it had.
    pub fn remove_account(&self, account: &str) {
        let _ = self.conn.execute("DELETE FROM locs WHERE account=?1", [account]);
        let _ = self.conn.execute("DELETE FROM folders WHERE account=?1", [account]);
        let _ = self.conn.execute_batch(&format!(
            "DELETE FROM raw WHERE mail IN (SELECT id FROM mails WHERE account='{0}' AND id NOT IN (SELECT mail FROM locs));
             DELETE FROM mails WHERE account='{0}' AND id NOT IN (SELECT mail FROM locs);",
            account.replace('\'', "''")
        ));
        self.drop_orphans();
    }

    pub fn flags(&self) -> HashMap<String, Flags> {
        let Ok(mut stmt) = self.conn.prepare("SELECT key, favorite, muted, ignored, spam FROM contacts") else { return HashMap::new() };
        stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, Flags { favorite: r.get(1)?, muted: r.get(2)?, ignored: r.get(3)?, spam: r.get(4)? })))
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default()
    }

    pub fn set_flag(&self, key: &str, flag: Flag, on: bool) {
        let col = match flag {
            Flag::Favorite => "favorite",
            Flag::Muted => "muted",
            Flag::Ignored => "ignored",
        };
        let _ = self.conn.execute(&format!("INSERT INTO contacts(key, {col}) VALUES(?1, ?2) ON CONFLICT(key) DO UPDATE SET {col}=?2"), params![key, on]);
    }

    /// Senders whose remote images load without asking.
    pub fn remote_image_senders(&self) -> HashSet<String> {
        let Ok(mut stmt) = self.conn.prepare("SELECT key FROM contacts WHERE remote_images=1") else { return HashSet::new() };
        stmt.query_map([], |r| r.get(0)).map(|rows| rows.flatten().collect()).unwrap_or_default()
    }

    pub fn set_remote_images(&self, sender: &str, on: bool) {
        let _ = self.conn.execute("INSERT INTO contacts(key, remote_images) VALUES(?1, ?2) ON CONFLICT(key) DO UPDATE SET remote_images=?2", params![sender, on]);
    }

    pub fn set_spam(&self, key: &str, value: i32) {
        let _ = self.conn.execute("INSERT INTO contacts(key, spam) VALUES(?1, ?2) ON CONFLICT(key) DO UPDATE SET spam=?2", params![key, value]);
    }

    /// Whether a mail lives only in Junk folders.
    pub fn is_junk(&self, id: i64) -> bool {
        self.conn.query_row("SELECT junk FROM mails WHERE id=?1", [id], |r| r.get(0)).unwrap_or(false)
    }

    pub fn mail_brief(&self, id: i64) -> Option<(String, String, String, String, bool, bool)> {
        self.conn
            .query_row("SELECT peer, from_addr, from_name, CASE WHEN has_body=1 THEN snippet ELSE subject END, outgoing, seen FROM mails WHERE id=?1", [id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
            })
            .optional()
            .ok()
            .flatten()
    }

    pub fn set_body(&self, id: i64, raw: &[u8], b: &Body) -> rusqlite::Result<()> {
        self.conn.execute("INSERT OR REPLACE INTO raw(mail, data) VALUES(?1, ?2)", params![id, raw])?;
        self.conn.execute(
            "UPDATE mails SET has_body=1, text=?2, quoted=?3, has_html=?4, attachments=?5, refs=?6, snippet=?7, rich=?8 WHERE id=?1",
            params![id, b.text, b.quoted, b.has_html, serde_json::to_string(&b.attachments).unwrap_or_default(), b.refs, b.snippet, b.rich],
        )?;
        Ok(())
    }

    /// Marks a body as permanently unavailable so the prefetcher stops asking.
    pub fn body_failed(&self, id: i64) {
        let _ = self.conn.execute("UPDATE mails SET has_body=-1 WHERE id=?1", [id]);
    }

    pub fn parse_version(&self) -> i32 {
        self.conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap_or(0)
    }

    /// Re-parses every stored body with the current parser.
    pub fn reparse_all(&self) -> rusqlite::Result<()> {
        let ids: Vec<i64> = {
            let mut stmt = self.conn.prepare("SELECT mail FROM raw")?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            rows.flatten().collect()
        };
        for chunk in ids.chunks(200) {
            // Parse first, then write quickly: the sync threads write to the same database.
            let bodies: Vec<(i64, Body)> = chunk.iter().filter_map(|&id| Some((id, crate::parse::body(&self.raw(id)?)))).collect();
            self.conn.execute_batch("BEGIN IMMEDIATE")?;
            for (id, body) in &bodies {
                self.conn.execute(
                    "UPDATE mails SET text=?2, quoted=?3, has_html=?4, attachments=?5, refs=?6, snippet=?7, rich=?8 WHERE id=?1",
                    params![id, body.text, body.quoted, body.has_html, serde_json::to_string(&body.attachments).unwrap_or_default(), body.refs, body.snippet, body.rich],
                )?;
            }
            self.conn.execute_batch("COMMIT")?;
        }
        self.conn.execute_batch(&format!("PRAGMA user_version = {PARSE_VERSION}"))
    }

    pub fn raw(&self, id: i64) -> Option<Vec<u8>> {
        self.conn.query_row("SELECT data FROM raw WHERE mail=?1", [id], |r| r.get(0)).optional().ok().flatten()
    }

    pub fn set_seen_flags(&self, account: &str, folder: &str, flags: &[(u32, bool)]) -> rusqlite::Result<usize> {
        let mut changed = 0;
        let mut stmt = self.conn.prepare_cached("UPDATE mails SET seen=?1 WHERE id=(SELECT mail FROM locs WHERE account=?2 AND folder=?3 AND uid=?4) AND seen<>?1 AND outgoing=0")?;
        for (uid, seen) in flags {
            changed += stmt.execute(params![seen, account, folder, uid])?;
        }
        Ok(changed)
    }

    pub fn known_uids(&self, account: &str, folder: &str) -> HashSet<u32> {
        let mut stmt = match self.conn.prepare("SELECT uid FROM locs WHERE account=?1 AND folder=?2") {
            Ok(s) => s,
            Err(_) => return HashSet::new(),
        };
        stmt.query_map(params![account, folder], |r| r.get(0)).map(|rows| rows.flatten().collect()).unwrap_or_default()
    }

    pub fn remove_uids(&self, account: &str, folder: &str, uids: &[u32]) {
        let mut ids = Vec::new();
        for uid in uids {
            if let Ok(Some(id)) = self.conn.query_row("SELECT mail FROM locs WHERE account=?1 AND folder=?2 AND uid=?3", params![account, folder, uid], |r| r.get::<_, i64>(0)).optional() {
                ids.push(id);
            }
            let _ = self.conn.execute("DELETE FROM locs WHERE account=?1 AND folder=?2 AND uid=?3", params![account, folder, uid]);
        }
        if !uids.is_empty() {
            // Moved out of (or into) Junk elsewhere: the remaining copies decide.
            for id in ids {
                let _ = self.conn.execute(&format!("UPDATE mails AS m SET junk = {JUNK_EXPR} WHERE m.id=?1"), [id]);
            }
            self.drop_orphans();
        }
    }

    /// Remembers which folders of an account are Junk folders.
    pub fn set_junk_folders(&self, account: &str, names: &[String]) {
        let _ = self.conn.execute("DELETE FROM junk_folders WHERE account=?1", [account]);
        for n in names {
            let _ = self.conn.execute("INSERT OR IGNORE INTO junk_folders(account, name) VALUES(?1, ?2)", params![account, n]);
        }
        let _ = self.conn.execute(&format!("UPDATE mails AS m SET junk = {JUNK_EXPR} WHERE m.id IN (SELECT mail FROM locs WHERE account=?1)"), [account]);
    }

    /// Recomputes the Junk state of these mails.
    pub fn refresh_junk(&self, ids: &[i64]) {
        let Ok(mut stmt) = self.conn.prepare(&format!("UPDATE mails AS m SET junk = {JUNK_EXPR} WHERE m.id=?1")) else { return };
        for id in ids {
            let _ = stmt.execute([id]);
        }
    }

    /// Mails of `account` still missing a body, newest first.
    pub fn missing_bodies(&self, account: &str, max_size: i64, since: i64, limit: usize) -> Vec<(i64, String, u32)> {
        let mut stmt = match self.conn.prepare(
            "SELECT m.id, l.folder, l.uid FROM mails m JOIN locs l ON l.mail = m.id
             WHERE l.account=?1 AND m.has_body=0 AND m.size <= ?2 AND m.date >= ?4 GROUP BY m.id ORDER BY m.date DESC LIMIT ?3",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        stmt.query_map(params![account, max_size, limit as i64, since], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default()
    }

    pub fn locations(&self, id: i64) -> Vec<(String, String, u32)> {
        let mut stmt = match self.conn.prepare("SELECT account, folder, uid FROM locs WHERE mail=?1") {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        stmt.query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).map(|rows| rows.flatten().collect()).unwrap_or_default()
    }

    pub fn names(&self) -> HashMap<String, String> {
        let mut stmt = match self.conn.prepare("SELECT addr, name FROM names") {
            Ok(s) => s,
            Err(_) => return HashMap::new(),
        };
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).map(|rows| rows.flatten().collect()).unwrap_or_default()
    }

    pub fn import_names(&self, names: &HashMap<String, String>) {
        let _ = self.conn.execute_batch("BEGIN IMMEDIATE");
        for (addr, name) in names {
            let _ = self.conn.execute(
                "INSERT INTO names(addr, name, prio) VALUES(?1, ?2, 3) ON CONFLICT(addr) DO UPDATE SET name=?2, prio=3",
                params![addr, name],
            );
        }
        let _ = self.conn.execute_batch("COMMIT");
    }

    /// One row per conversation, newest first. With `account`, only mail of that account counts.
    pub fn peers(&self, account: Option<&str>) -> Vec<PeerRow> {
        let sql = format!(
            "SELECT peer, MAX(date), snippet, outgoing, SUM(seen = 0 AND outgoing = 0), SUM(junk) = COUNT(*), GROUP_CONCAT(me_addr, ' ')
             FROM (SELECT peer, date, snippet, outgoing, seen, junk, me_addr, account FROM mails ORDER BY date DESC) {} GROUP BY peer ORDER BY 2 DESC",
            if account.is_some() { "WHERE account = ?1" } else { "" }
        );
        let Ok(mut stmt) = self.conn.prepare(&sql) else { return Vec::new() };
        let map = |r: &rusqlite::Row| {
            let mut me_addrs: Vec<String> = Vec::new();
            for a in r.get::<_, String>(6)?.split(' ') {
                if !a.is_empty() && !me_addrs.iter().any(|x| x == a) {
                    me_addrs.push(a.to_string());
                }
            }
            Ok(PeerRow { peer: r.get(0)?, last_date: r.get(1)?, snippet: r.get(2)?, outgoing: r.get(3)?, unread: r.get(4)?, junk: r.get(5)?, me_addrs })
        };
        let rows = match account {
            Some(a) => stmt.query_map([a], map),
            None => stmt.query_map([], map),
        };
        rows.map(|r| r.flatten().collect()).unwrap_or_default()
    }

    /// Conversations whose mail mentions `needle` in subject or text.
    pub fn peers_matching(&self, needle: &str) -> HashSet<String> {
        let like = format!("%{}%", needle.replace('%', "\\%").replace('_', "\\_"));
        let Ok(mut stmt) = self.conn.prepare("SELECT DISTINCT peer FROM mails WHERE subject LIKE ?1 ESCAPE '\\' OR text LIKE ?1 ESCAPE '\\'") else {
            return HashSet::new();
        };
        stmt.query_map([like], |r| r.get(0)).map(|rows| rows.flatten().collect()).unwrap_or_default()
    }

    pub fn conversation(&self, peer: &str, limit: usize) -> (Vec<MailRow>, i64) {
        let filter = conversation_filter(peer);
        let total: i64 = self.conn.query_row(&format!("SELECT COUNT(*) FROM mails WHERE {filter}"), [peer], |r| r.get(0)).unwrap_or(0);
        let Ok(mut stmt) = self.conn.prepare(&format!(
            "SELECT id, msgid, date, from_addr, from_name, subject, outgoing, seen, account, me_addr, has_body, text, quoted, has_html, attachments, refs, in_reply_to, peer, recipients, junk, rich
             FROM mails WHERE {filter} ORDER BY date DESC LIMIT ?2"
        )) else {
            return (Vec::new(), total);
        };
        let rows = stmt
            .query_map(params![peer, limit as i64], |r| {
                Ok(MailRow {
                    id: r.get(0)?,
                    msgid: r.get(1)?,
                    date: r.get(2)?,
                    from: Person { addr: r.get(3)?, name: r.get(4)? },
                    subject: r.get(5)?,
                    outgoing: r.get(6)?,
                    seen: r.get(7)?,
                    account: r.get(8)?,
                    me_addr: r.get(9)?,
                    has_body: r.get::<_, i64>(10)? == 1,
                    text: r.get(11)?,
                    quoted: r.get(12)?,
                    has_html: r.get(13)?,
                    attachments: serde_json::from_str(&r.get::<_, String>(14)?).unwrap_or_default(),
                    refs: r.get(15)?,
                    in_reply_to: r.get(16)?,
                    peer: r.get(17)?,
                    recipients: serde_json::from_str(&r.get::<_, String>(18)?).unwrap_or_default(),
                    junk: r.get(19)?,
                    rich: r.get(20)?,
                })
            })
            .map(|rows| rows.flatten().collect::<Vec<_>>())
            .unwrap_or_default();
        let mut rows = rows;
        rows.reverse();
        (rows, total)
    }

    /// Marks the conversation read locally; returns the server locations to flag \Seen.
    pub fn mark_peer_seen(&self, peer: &str) -> Vec<(String, String, u32)> {
        let mut out = Vec::new();
        let filter = conversation_filter(peer).replace("peer", "m.peer");
        if let Ok(mut stmt) = self.conn.prepare(&format!("SELECT l.account, l.folder, l.uid FROM mails m JOIN locs l ON l.mail=m.id WHERE {filter} AND m.seen=0")) {
            if let Ok(rows) = stmt.query_map([peer], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))) {
                out = rows.flatten().collect();
            }
        }
        let _ = self.conn.execute(&format!("UPDATE mails SET seen=1 WHERE {} AND seen=0", conversation_filter(peer)), [peer]);
        out
    }

    pub fn mark_peer_unseen(&self, peer: &str) -> Vec<(String, String, u32)> {
        let id: Option<i64> = self.conn.query_row("SELECT id FROM mails WHERE peer=?1 AND outgoing=0 ORDER BY date DESC LIMIT 1", [peer], |r| r.get(0)).optional().ok().flatten();
        let Some(id) = id else { return Vec::new() };
        let _ = self.conn.execute("UPDATE mails SET seen=0 WHERE id=?1", [id]);
        self.locations(id)
    }

    /// Stores a mail we just sent, before the server's Sent folder has it.
    pub fn add_sent(&self, account: &str, h: &Header, p: &Placement, raw: &[u8], b: &Body) -> rusqlite::Result<i64> {
        let mut recipients = h.to.clone();
        recipients.extend(h.cc.iter().cloned());
        self.conn.execute(
            "INSERT OR IGNORE INTO mails(msgid, date, from_addr, from_name, recipients, subject, peer, outgoing, seen, in_reply_to, account, me_addr, size)
             VALUES(?1,?2,?3,?4,?5,?6,?7,1,1,?8,?9,?10,?11)",
            params![h.msgid, h.date, h.from.addr.to_lowercase(), h.from.name, serde_json::to_string(&recipients).unwrap_or_default(), h.subject, p.peer, h.in_reply_to, account, p.me_addr, raw.len() as i64],
        )?;
        let id: i64 = self.conn.query_row("SELECT id FROM mails WHERE msgid=?1", [&h.msgid], |r| r.get(0))?;
        self.set_body(id, raw, b)?;
        Ok(id)
    }
}
