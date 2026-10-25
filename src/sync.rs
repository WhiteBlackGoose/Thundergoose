//! One background thread per account keeps the local store in step with the IMAP server.
//!
//! New mail is fetched first, then history is backfilled newest-to-oldest in chunks, then bodies
//! are prefetched. Requests from the UI (open a conversation, mark read, upload a sent mail) are
//! served between chunks so they never wait for a full sync.

use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use imap_proto::NameAttribute;

use crate::config::{Account, Paths};
use crate::creds::Creds;
use crate::imapc::{self, ConnError, Imap};
use crate::store::{self, Store};

const ENVELOPE_CHUNK: usize = 400;
const BODY_CHUNK: usize = 20;
const PREFETCH_MAX_SIZE: i64 = 2 * 1024 * 1024;
/// Bodies older than this are only downloaded when their conversation is opened.
const PREFETCH_DAYS: i64 = 365;
const POLL: Duration = Duration::from_secs(60);
const FULL_RESCAN: Duration = Duration::from_secs(15 * 60);
/// Read state is refreshed for this many of the newest mails per folder on a full scan.
const FLAG_WINDOW: usize = 5000;
/// New mail older than this doesn't notify (e.g. after a long time offline).
const NOTIFY_MAX_AGE: i64 = 2 * 86400;

pub enum Cmd {
    /// Fetch these bodies now (a conversation was opened).
    Bodies(Vec<i64>),
    /// Set or clear \Seen at these locations.
    Flag(Vec<(String, u32)>, bool),
    /// Upload a sent message to the Sent folder.
    AppendSent(Vec<u8>),
    /// Check for new mail now.
    Wake,
    /// Credentials changed; reconnect.
    Reconnect,
    /// The account was removed.
    Stop,
}

/// The user's own addresses, shared so that adding an account updates every thread.
pub type Me = Arc<RwLock<HashSet<String>>>;

#[derive(Clone, Debug, Default)]
pub struct Status {
    pub online: bool,
    pub needs_login: bool,
    pub busy: bool,
    pub message: String,
}

/// State shared between the sync threads and the UI.
pub struct Shared {
    pub dirty: AtomicBool,
    /// Mail that just arrived and may deserve a notification.
    pub fresh: Mutex<Vec<i64>>,
    pub status: Mutex<Vec<(String, Status)>>,
}

impl Shared {
    pub fn new() -> Self {
        Shared { dirty: AtomicBool::new(false), fresh: Mutex::new(Vec::new()), status: Mutex::new(Vec::new()) }
    }

    fn set_status(&self, account: &str, f: impl FnOnce(&mut Status)) {
        let mut all = self.status.lock().unwrap();
        if let Some((_, s)) = all.iter_mut().find(|(a, _)| a == account) {
            f(s);
        } else {
            let mut s = Status::default();
            f(&mut s);
            all.push((account.to_string(), s));
        }
        self.dirty.store(true, Ordering::Relaxed);
    }
}

#[derive(Clone, Debug)]
struct Folder {
    name: String,
    sent: bool,
}

/// Names of Junk folders on servers that don't mark them.
const JUNK_NAMES: &[&str] = &["junk", "junk e-mail", "junk-e-mail", "junk email", "spam", "bulk mail", "spamverdacht", "unerwünscht"];

const SKIP_NAMES: &[&str] = &[
    "trash", "deleted", "deleted items", "deleted messages", "bin", "papierkorb", "gelöschte elemente", "junk", "junk e-mail", "junk-e-mail", "junk email",
    "spam", "drafts", "entwürfe", "outbox", "postausgang", "calendar", "kalender", "contacts", "kontakte", "tasks", "aufgaben", "journal", "notes", "notizen",
    "conversation history", "verlauf der unterhaltung", "sync issues", "synchronisierungsprobleme", "rss feeds", "rss-feeds", "templates", "vorlagen",
];

/// Decodes IMAP's modified UTF-7 folder names.
fn utf7_imap(name: &str) -> String {
    use base64::Engine as _;
    let mut out = String::new();
    let mut rest = name;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        let tail = &rest[start + 1..];
        let Some(end) = tail.find('-') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let enc = &tail[..end];
        if enc.is_empty() {
            out.push('&');
        } else {
            let b64 = enc.replace(',', "/");
            let bytes = base64::engine::general_purpose::STANDARD_NO_PAD.decode(b64.trim_end_matches('=')).unwrap_or_default();
            let units: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
            out.push_str(&String::from_utf16_lossy(&units));
        }
        rest = &tail[end + 1..];
    }
    out.push_str(rest);
    out
}

/// The folders to sync, and which of them are Junk folders (synced last, for the Spam tab).
fn choose_folders(s: &mut Imap) -> Result<(Vec<Folder>, Vec<String>), imap::Error> {
    let names = s.list(Some(""), Some("*"))?;
    let has = |n: &imap::types::Name<'_>, a: NameAttribute<'_>| n.attributes().contains(&a);
    let leaf_of = |n: &imap::types::Name<'_>| {
        let name = n.name();
        utf7_imap(name.rsplit(n.delimiter().unwrap_or("/")).next().unwrap_or(name)).to_lowercase()
    };
    let junk: Vec<String> = names
        .iter()
        .find(|n| has(n, NameAttribute::Junk))
        .or_else(|| names.iter().find(|n| !has(n, NameAttribute::NoSelect) && JUNK_NAMES.contains(&leaf_of(n).as_str())))
        .map(|n| vec![n.name().to_string()])
        .unwrap_or_default();
    let junk_folders = || junk.iter().map(|n| Folder { name: n.clone(), sent: false });
    // Gmail: All Mail holds everything except spam and trash, including sent mail.
    if let Some(all) = names.iter().find(|n| has(n, NameAttribute::All)) {
        let mut out = vec![Folder { name: all.name().to_string(), sent: false }];
        out.extend(junk_folders());
        return Ok((out, junk));
    }
    let mut skipped: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for n in names.iter() {
        let name = n.name();
        let delim = n.delimiter().unwrap_or("/");
        let leaf = utf7_imap(name.rsplit(delim).next().unwrap_or(name)).to_lowercase();
        let special_skip = [NameAttribute::NoSelect, NameAttribute::Junk, NameAttribute::Trash, NameAttribute::Drafts].into_iter().any(|a| has(n, a));
        let under_skipped = skipped.iter().any(|p| name.starts_with(&format!("{p}{delim}")));
        if special_skip || under_skipped || SKIP_NAMES.contains(&leaf.as_str()) {
            if !has(n, NameAttribute::NoSelect) {
                skipped.push(name.to_string());
            }
            continue;
        }
        out.push(Folder { name: name.to_string(), sent: has(n, NameAttribute::Sent) });
    }
    // INBOX first, then Sent, then the rest.
    out.sort_by_key(|f| (!f.name.eq_ignore_ascii_case("INBOX"), !f.sent));
    out.extend(junk_folders());
    Ok((out, junk))
}

fn find_sent(s: &mut Imap) -> Option<String> {
    let names = s.list(Some(""), Some("*")).ok()?;
    names
        .iter()
        .find(|n| n.attributes().contains(&NameAttribute::Sent))
        .or_else(|| names.iter().find(|n| ["sent", "sent items", "sent messages", "gesendet", "gesendete elemente"].contains(&utf7_imap(n.name()).rsplit(['/', '.']).next().unwrap_or("").to_lowercase().as_str())))
        .map(|n| n.name().to_string())
}

fn uid_set(uids: &[u32]) -> String {
    // Compress consecutive runs: 1,2,3,7 -> 1:3,7
    let mut parts = Vec::new();
    let mut i = 0;
    while i < uids.len() {
        let start = uids[i];
        let mut end = start;
        while i + 1 < uids.len() && uids[i + 1] == end + 1 {
            i += 1;
            end = uids[i];
        }
        parts.push(if start == end { start.to_string() } else { format!("{start}:{end}") });
        i += 1;
    }
    parts.join(",")
}

struct Worker {
    acct: Account,
    me: Me,
    creds: Arc<Creds>,
    shared: Arc<Shared>,
    store: Store,
    rx: Receiver<Cmd>,
    pending: VecDeque<Cmd>,
    /// Server UIDs per folder from the last full scan, sorted ascending.
    uids: Vec<(String, Vec<u32>)>,
    last_full: Option<Instant>,
    stopped: bool,
}

enum Flow {
    Continue,
    Reconnect,
}

macro_rules! serve_or_return {
    ($self:ident, $s:ident) => {
        if let Flow::Reconnect = $self.serve($s)? {
            return Ok(());
        }
    };
}

impl Worker {
    fn status(&self, f: impl FnOnce(&mut Status)) {
        self.shared.set_status(&self.acct.id, f);
    }

    fn take_cmds(&mut self) {
        loop {
            match self.rx.try_recv() {
                Ok(c) => self.pending.push_back(c),
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.stopped = true;
                    break;
                }
            }
        }
    }

    fn run(mut self) {
        let mut backoff = 5;
        loop {
            if self.stopped {
                return;
            }
            self.status(|s| {
                s.busy = true;
                s.message = "Connecting…".into();
            });
            match imapc::connect(&self.acct, &self.creds) {
                Ok(mut session) => {
                    backoff = 5;
                    self.status(|s| {
                        s.online = true;
                        s.needs_login = false;
                        s.message.clear();
                    });
                    let result = self.session(&mut session);
                    let _ = session.logout();
                    if self.stopped {
                        return;
                    }
                    if let Err(e) = result {
                        self.status(|s| {
                            s.online = false;
                            s.busy = false;
                            s.message = format!("Connection lost: {e}");
                        });
                    }
                }
                Err(ConnError::NeedsLogin(m)) | Err(ConnError::Auth(m)) => {
                    self.status(|s| {
                        s.online = false;
                        s.busy = false;
                        s.needs_login = true;
                        s.message = m;
                    });
                    // Wait until the user signs in again.
                    loop {
                        match self.rx.recv() {
                            Ok(Cmd::Reconnect) => break,
                            Ok(Cmd::Stop) => return,
                            Ok(other) => {
                                if !matches!(other, Cmd::Wake) {
                                    self.pending.push_back(other);
                                }
                            }
                            Err(_) => return,
                        }
                    }
                    continue;
                }
                Err(ConnError::Net(m)) => {
                    self.status(|s| {
                        s.online = false;
                        s.busy = false;
                        s.message = m;
                    });
                }
            }
            // Offline: wait with backoff, but come back early on request.
            match self.rx.recv_timeout(Duration::from_secs(backoff)) {
                Ok(Cmd::Wake) | Ok(Cmd::Reconnect) => {}
                Ok(Cmd::Stop) => return,
                Ok(other) => self.pending.push_back(other),
                Err(RecvTimeoutError::Disconnected) => return,
                Err(RecvTimeoutError::Timeout) => {}
            }
            backoff = (backoff * 2).min(300);
        }
    }

    fn session(&mut self, s: &mut Imap) -> Result<(), imap::Error> {
        let (folders, junk) = choose_folders(s)?;
        self.store.set_junk_folders(&self.acct.id, &junk);
        loop {
            self.status(|st| st.busy = true);
            let full = self.last_full.is_none_or(|t| t.elapsed() > FULL_RESCAN);
            for f in &folders {
                self.sync_new(s, f, full)?;
                serve_or_return!(self, s);
            }
            if full {
                self.last_full = Some(Instant::now());
            }
            // Walk back in time: a chunk of older headers per folder, then a batch of the
            // newest missing bodies, so recent mail is readable long before history is complete.
            loop {
                let mut more = false;
                for f in &folders {
                    more |= self.backfill(s, f)?;
                    serve_or_return!(self, s);
                }
                more |= self.prefetch(s)?;
                serve_or_return!(self, s);
                if !more {
                    break;
                }
            }
            self.status(|st| {
                st.busy = false;
                st.message.clear();
            });
            // Idle until the next poll or a request.
            let deadline = Instant::now() + POLL;
            loop {
                let now = Instant::now();
                if now >= deadline {
                    break;
                }
                match self.rx.recv_timeout(deadline - now) {
                    Ok(Cmd::Wake) => break,
                    Ok(c) => {
                        self.pending.push_back(c);
                        serve_or_return!(self, s);
                    }
                    Err(RecvTimeoutError::Timeout) => break,
                    Err(RecvTimeoutError::Disconnected) => {
                        self.stopped = true;
                        return Ok(());
                    }
                }
            }
            s.noop()?;
        }
    }

    /// Handles queued UI requests.
    fn serve(&mut self, s: &mut Imap) -> Result<Flow, imap::Error> {
        self.take_cmds();
        if self.stopped {
            return Ok(Flow::Reconnect);
        }
        while let Some(cmd) = self.pending.pop_front() {
            match cmd {
                Cmd::Bodies(ids) => {
                    let mut by_folder: Vec<(String, Vec<(u32, i64)>)> = Vec::new();
                    for id in ids {
                        if let Some((_, folder, uid)) = self.store.locations(id).into_iter().find(|(a, _, _)| *a == self.acct.id) {
                            match by_folder.iter_mut().find(|(f, _)| *f == folder) {
                                Some((_, v)) => v.push((uid, id)),
                                None => by_folder.push((folder, vec![(uid, id)])),
                            }
                        }
                    }
                    for (folder, items) in by_folder {
                        self.fetch_bodies(s, &folder, &items)?;
                    }
                }
                Cmd::Flag(locs, seen) => {
                    let mut by_folder: Vec<(String, Vec<u32>)> = Vec::new();
                    for (folder, uid) in locs {
                        match by_folder.iter_mut().find(|(f, _)| *f == folder) {
                            Some((_, v)) => v.push(uid),
                            None => by_folder.push((folder, vec![uid])),
                        }
                    }
                    for (folder, mut uids) in by_folder {
                        uids.sort_unstable();
                        s.select(&folder)?;
                        let op = if seen { "+FLAGS.SILENT (\\Seen)" } else { "-FLAGS.SILENT (\\Seen)" };
                        s.uid_store(uid_set(&uids), op)?;
                    }
                }
                Cmd::AppendSent(raw) => {
                    if let Some(sent) = find_sent(s) {
                        s.append(&sent, &raw).flag(imap::types::Flag::Seen).finish()?;
                    }
                }
                Cmd::Wake => {}
                Cmd::Reconnect => return Ok(Flow::Reconnect),
                Cmd::Stop => {
                    self.stopped = true;
                    return Ok(Flow::Reconnect);
                }
            }
        }
        Ok(Flow::Continue)
    }

    /// Stores fetched headers. With `notify`, new unread incoming mail is queued for notifications.
    fn insert_fetches(&self, folder: &str, fetches: &imap::types::Fetches, notify: bool) {
        let me = self.me.read().unwrap().clone();
        let now = chrono::Utc::now().timestamp();
        let mut fresh = Vec::new();
        let mut added = Vec::new();
        let _ = self.store.conn.execute_batch("BEGIN");
        for f in fetches.iter() {
            let (Some(uid), Some(h)) = (f.uid, crate::parse::header_from_fetch(f)) else { continue };
            let p = store::place(&h, &me, &self.acct.email);
            match self.store.add(&self.acct.id, folder, uid, &h, &p) {
                Ok((id, inserted)) => {
                    added.push(id);
                    if inserted && notify && !p.outgoing && !h.seen && now - h.date < NOTIFY_MAX_AGE {
                        fresh.push(id);
                    }
                }
                Err(e) => eprintln!("store: {e}"),
            }
        }
        self.store.refresh_junk(&added);
        let _ = self.store.conn.execute_batch("COMMIT");
        if !fresh.is_empty() {
            self.shared.fresh.lock().unwrap().extend(fresh);
        }
        self.shared.dirty.store(true, Ordering::Relaxed);
    }

    fn fetch_envelopes(&self, s: &mut Imap, folder: &str, set: &str) -> Result<(), imap::Error> {
        let fetches = s.uid_fetch(set, "(UID FLAGS RFC822.SIZE INTERNALDATE ENVELOPE)")?;
        self.insert_fetches(folder, &fetches, false);
        Ok(())
    }

    /// New mail, flag changes of recent mail and (on full scans) deletions.
    fn sync_new(&mut self, s: &mut Imap, f: &Folder, full: bool) -> Result<(), imap::Error> {
        let mbox = s.select(&f.name)?;
        let validity = mbox.uid_validity.unwrap_or(0);
        let uid_next = mbox.uid_next.unwrap_or(0);
        let state = self.store.folder_state(&self.acct.id, &f.name);
        let (high, low) = match state {
            Some((v, h, l)) if v == validity => (h, l),
            Some(_) => {
                self.store.reset_folder(&self.acct.id, &f.name);
                self.uids.retain(|(n, _)| *n != f.name);
                (0, 0)
            }
            None => (0, 0),
        };
        if mbox.exists == 0 {
            self.store.set_folder_state(&self.acct.id, &f.name, validity, uid_next.saturating_sub(1), 1);
            return Ok(());
        }
        if full || !self.uids.iter().any(|(n, _)| *n == f.name) {
            let mut all: Vec<u32> = s.uid_search("ALL")?.into_iter().collect();
            all.sort_unstable();
            let present: HashSet<u32> = all.iter().copied().collect();
            let gone: Vec<u32> = self.store.known_uids(&self.acct.id, &f.name).into_iter().filter(|u| !present.contains(u)).collect();
            self.store.remove_uids(&self.acct.id, &f.name, &gone);
            // Refresh read state of the newest mail we know about.
            if high > 0 {
                let mut known: Vec<u32> = all.iter().copied().filter(|&u| u >= low && u <= high).collect();
                known.drain(..known.len().saturating_sub(FLAG_WINDOW));
                for chunk in known.chunks(2000) {
                    let fl = s.uid_fetch(uid_set(chunk), "(UID FLAGS)")?;
                    let flags: Vec<(u32, bool)> = fl.iter().filter_map(|x| Some((x.uid?, x.flags().iter().any(|f| matches!(f, imap::types::Flag::Seen))))).collect();
                    if self.store.set_seen_flags(&self.acct.id, &f.name, &flags).unwrap_or(0) > 0 {
                        self.shared.dirty.store(true, Ordering::Relaxed);
                    }
                }
            }
            self.uids.retain(|(n, _)| *n != f.name);
            self.uids.push((f.name.clone(), all));
        } else if high > 0 {
            // Cheap flag refresh of the newest mail.
            let from = high.saturating_sub(300).max(1);
            let fl = s.uid_fetch(format!("{from}:{high}"), "(UID FLAGS)")?;
            let flags: Vec<(u32, bool)> = fl.iter().filter_map(|x| Some((x.uid?, x.flags().iter().any(|f| matches!(f, imap::types::Flag::Seen))))).collect();
            if self.store.set_seen_flags(&self.acct.id, &f.name, &flags).unwrap_or(0) > 0 {
                self.shared.dirty.store(true, Ordering::Relaxed);
            }
        }

        if high == 0 {
            // First sight of this folder: start backfilling from the top.
            let top = self.uids.iter().find(|(n, _)| *n == f.name).and_then(|(_, u)| u.last().copied()).unwrap_or(uid_next.saturating_sub(1));
            self.store.set_folder_state(&self.acct.id, &f.name, validity, top, top + 1);
            return Ok(());
        }
        if uid_next > high + 1 {
            let fetches = s.uid_fetch(format!("{}:*", high + 1), "(UID FLAGS RFC822.SIZE INTERNALDATE ENVELOPE)")?;
            let new_high = fetches.iter().filter_map(|f| f.uid).max().unwrap_or(high).max(high);
            self.insert_fetches(&f.name, &fetches, true);
            if let Some((_, all)) = self.uids.iter_mut().find(|(n, _)| *n == f.name) {
                all.extend(fetches.iter().filter_map(|f| f.uid).filter(|&u| u > high));
                all.sort_unstable();
                all.dedup();
            }
            self.store.set_folder_state(&self.acct.id, &f.name, validity, new_high, low);
        }
        Ok(())
    }

    /// Fetches one chunk of older history. Returns whether more remains.
    fn backfill(&mut self, s: &mut Imap, f: &Folder) -> Result<bool, imap::Error> {
        let Some((validity, high, low)) = self.store.folder_state(&self.acct.id, &f.name) else { return Ok(false) };
        if low <= 1 {
            return Ok(false);
        }
        let Some((_, all)) = self.uids.iter().find(|(n, _)| *n == f.name) else { return Ok(false) };
        let below: Vec<u32> = all.iter().copied().filter(|&u| u < low).collect();
        if below.is_empty() {
            self.store.set_folder_state(&self.acct.id, &f.name, validity, high, 1);
            return Ok(false);
        }
        let chunk = &below[below.len().saturating_sub(ENVELOPE_CHUNK)..];
        let new_low = chunk[0];
        self.status(|st| st.message = format!("Fetching history ({} left in {})", below.len(), utf7_imap(&f.name)));
        s.select(&f.name)?;
        self.fetch_envelopes(s, &f.name, &uid_set(chunk))?;
        self.store.set_folder_state(&self.acct.id, &f.name, validity, high, if chunk.len() == below.len() { 1 } else { new_low });
        Ok(chunk.len() < below.len())
    }

    fn fetch_bodies(&self, s: &mut Imap, folder: &str, items: &[(u32, i64)]) -> Result<(), imap::Error> {
        let mut uids: Vec<u32> = items.iter().map(|(u, _)| *u).collect();
        uids.sort_unstable();
        s.select(folder)?;
        let fetches = s.uid_fetch(uid_set(&uids), "(UID BODY.PEEK[])")?;
        let mut got = HashSet::new();
        for f in fetches.iter() {
            let (Some(uid), Some(raw)) = (f.uid, f.body()) else { continue };
            let Some(&(_, id)) = items.iter().find(|(u, _)| *u == uid) else { continue };
            let body = crate::parse::body(raw);
            if let Err(e) = self.store.set_body(id, raw, &body) {
                eprintln!("store body: {e}");
            }
            got.insert(id);
        }
        for (_, id) in items {
            if !got.contains(id) {
                self.store.body_failed(*id);
            }
        }
        self.shared.dirty.store(true, Ordering::Relaxed);
        Ok(())
    }

    /// Downloads a batch of missing bodies, newest first. Returns whether more remain.
    fn prefetch(&mut self, s: &mut Imap) -> Result<bool, imap::Error> {
        let since = chrono::Utc::now().timestamp() - PREFETCH_DAYS * 86400;
        let missing = self.store.missing_bodies(&self.acct.id, PREFETCH_MAX_SIZE, since, BODY_CHUNK);
        if missing.is_empty() {
            return Ok(false);
        }
        let mut by_folder: Vec<(String, Vec<(u32, i64)>)> = Vec::new();
        for (id, folder, uid) in missing {
            match by_folder.iter_mut().find(|(f, _)| *f == folder) {
                Some((_, v)) => v.push((uid, id)),
                None => by_folder.push((folder, vec![(uid, id)])),
            }
        }
        self.status(|st| st.message = "Downloading messages…".into());
        for (folder, items) in by_folder {
            self.fetch_bodies(s, &folder, &items)?;
        }
        Ok(true)
    }
}

pub fn me_addresses(accounts: &[Account]) -> HashSet<String> {
    accounts.iter().flat_map(|a| a.addresses().map(|s| s.to_lowercase())).collect()
}

pub fn spawn(acct: Account, me: Me, creds: Arc<Creds>, shared: Arc<Shared>, paths: &Paths) -> Sender<Cmd> {
    let (tx, rx) = std::sync::mpsc::channel();
    let db = paths.db();
    let (idle_acct, idle_creds) = (acct.clone(), creds.clone());
    shared.set_status(&acct.id, |s| s.message = "Starting…".into());
    std::thread::Builder::new()
        .name(format!("sync {}", acct.email))
        .spawn(move || {
            let store = match Store::open(&db) {
                Ok(s) => s,
                Err(e) => {
                    shared.set_status(&acct.id, |s| s.message = format!("Database error: {e}"));
                    return;
                }
            };
            Worker { acct, me, creds, shared, store, rx, pending: VecDeque::new(), uids: Vec::new(), last_full: None, stopped: false }.run();
        })
        .expect("spawn sync thread");
    let wake = tx.clone();
    std::thread::Builder::new().name("idle".into()).spawn(move || idle(&idle_acct, &idle_creds, &wake)).expect("spawn idle thread");
    tx
}

/// Keeps a second connection in IMAP IDLE on the INBOX, so the server tells us about new mail
/// the moment it arrives; the sync thread is woken up to fetch it. Servers without IDLE are
/// left to the regular poll.
fn idle(acct: &Account, creds: &Creds, wake: &Sender<Cmd>) {
    let mut backoff = 15;
    loop {
        if let Ok(mut s) = imapc::connect(acct, creds) {
            if !s.capabilities().is_ok_and(|c| c.has_str("IDLE")) {
                let _ = s.logout();
                return;
            }
            if s.select("INBOX").is_ok() {
                backoff = 15;
                // Refreshed every 20 minutes; returns when the mailbox changes.
                while let Ok(outcome) = s.idle().timeout(Duration::from_secs(20 * 60)).wait_while(imap::extensions::idle::stop_on_any) {
                    if matches!(outcome, imap::extensions::idle::WaitOutcome::MailboxChanged) && wake.send(Cmd::Wake).is_err() {
                        // The account was removed.
                        return;
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_secs(backoff));
        backoff = (backoff * 2).min(600);
    }
}
