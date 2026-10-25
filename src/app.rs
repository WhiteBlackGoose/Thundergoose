//! Glue between the Slint UI, the local store and the sync threads.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{Datelike, Local, TimeZone};
use slint::{ComponentHandle, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, VecModel};

use crate::config::{load_json, save_json, Account, Auth, Paths, Secrets, Settings};
use crate::creds::Creds;
use crate::config::{Issuer, Security, Server};
use crate::favicon::Favicons;
use crate::store::{Flag, Flags, MailRow, PeerRow, Person, Placement, Store};
use crate::sync::{self, Cmd, Shared};
use crate::{AccountItem, AccountTag, App, AppWindow, AttachmentItem, MessageItem, Palette, Participant, PeerItem, Theme};

const PAGE: usize = 60;
const MAX_INLINE_TEXT: usize = 6000;
/// Image attachments up to this size are shown in the conversation.
const MAX_PREVIEW_BYTES: usize = 10 * 1024 * 1024;
/// Longest side of a decoded preview, in pixels.
const PREVIEW_PX: u32 = 1200;

type PreviewKey = (i64, usize);
/// A rendering of an HTML mail: mail id, full (with quotes, uncut), remote images loaded.
type HtmlKey = (i64, bool, bool);
type HtmlJobs = Arc<Mutex<Vec<(HtmlKey, Option<crate::html::Rendered>)>>>;
/// Renderings kept in memory.
const HTML_CACHE: usize = 40;

struct HtmlView {
    image: slint::Image,
    links: Vec<crate::html::Link>,
    css_height: f32,
    blocked: bool,
    quoted: bool,
    cut: bool,
}

/// One thread renders HTML mail, newest requests first, so fonts load once.
fn spawn_html_renderer(db: PathBuf, jobs: HtmlJobs) -> Sender<(HtmlKey, f32)> {
    let (tx, rx) = std::sync::mpsc::channel::<(HtmlKey, f32)>();
    std::thread::Builder::new()
        .name("html".into())
        .spawn(move || {
            let Ok(store) = Store::open(&db) else { return };
            while let Ok((key, scale)) = rx.recv() {
                let (id, full, remote) = key;
                let raw = store.raw(id);
                // Blitz is young; a mail that trips it shows as text instead of taking the app down.
                let rendered = raw.and_then(|raw| {
                    let html = crate::parse::html(&raw)?;
                    let inline = crate::parse::inline_parts(&raw);
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| crate::html::render(&html, inline, remote, scale, full))).ok().flatten()
                });
                jobs.lock().unwrap().push((key, rendered));
            }
        })
        .expect("spawn html thread");
    tx
}
type PreviewJobs = Arc<Mutex<Vec<(PreviewKey, Option<SharedPixelBuffer<Rgba8Pixel>>)>>>;

fn previewable(a: &crate::store::Attachment) -> bool {
    a.size <= MAX_PREVIEW_BYTES && matches!(a.mime.as_str(), "image/png" | "image/jpeg" | "image/jpg" | "image/pjpeg" | "image/gif" | "image/webp" | "image/bmp")
}

/// Decodes an image attachment upright and no bigger than needed.
fn decode_preview(bytes: &[u8]) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    use image::ImageDecoder;
    let mut decoder = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()?.into_decoder().ok()?;
    let orientation = decoder.orientation().ok();
    let mut img = image::DynamicImage::from_decoder(decoder).ok()?;
    if let Some(o) = orientation {
        img.apply_orientation(o);
    }
    if img.width() > PREVIEW_PX || img.height() > PREVIEW_PX {
        img = img.thumbnail(PREVIEW_PX, PREVIEW_PX);
    }
    let rgba = img.to_rgba8();
    Some(SharedPixelBuffer::clone_from_slice(rgba.as_raw(), rgba.width(), rgba.height()))
}

/// The website icon for a one-person conversation, loading or requesting it as needed.
fn icon_for(icons: &mut HashMap<String, Option<slint::Image>>, favicons: &Favicons, enabled: bool, key: &str) -> Option<slint::Image> {
    if !enabled || key.is_empty() || key.contains(',') {
        return None;
    }
    let domain = crate::favicon::domain_for(key)?;
    icons
        .entry(domain.clone())
        .or_insert_with(|| match favicons.cached(&domain) {
            Some(path) => slint::Image::load_from_path(&path).ok(),
            None => {
                favicons.request(&domain);
                None
            }
        })
        .clone()
}

const HUES: [(u8, u8, u8); 10] = [
    (0x6a, 0x4c, 0xf5),
    (0x1f, 0x6f, 0xe5),
    (0x0d, 0x94, 0x88),
    (0xe8, 0x71, 0x0a),
    (0xe1, 0x1d, 0x48),
    (0x93, 0x33, 0xea),
    (0x08, 0x91, 0xb2),
    (0x65, 0xa3, 0x0d),
    (0xc2, 0x41, 0x0c),
    (0xdb, 0x27, 0x77),
];

/// Sidebar tab of a conversation: 0 Chats, 1 Muted, 2 Ignored, 3 Spam. `junk`: all its mail is
/// in Junk folders. Your own choices win over the server's spam filter.
fn tab_for(f: Flags, junk: bool) -> i32 {
    if f.ignored {
        2
    } else if f.spam == 1 {
        3
    } else if f.muted {
        1
    } else if junk && f.spam != -1 {
        3
    } else {
        0
    }
}

fn hue(key: &str) -> slint::Color {
    let h = key.bytes().fold(5381u32, |h, b| h.wrapping_mul(33) ^ b as u32);
    let (r, g, b) = HUES[(h % HUES.len() as u32) as usize];
    slint::Color::from_rgb_u8(r, g, b)
}

fn initials(name: &str) -> String {
    let words: Vec<&str> = name.split(|c: char| c.is_whitespace() || c == '.' || c == '_' || c == '-' || c == '@').filter(|w| w.chars().next().is_some_and(char::is_alphanumeric)).collect();
    let mut s: String = words.iter().take(2).filter_map(|w| w.chars().next()).flat_map(char::to_uppercase).collect();
    if s.is_empty() {
        s = "?".into();
    }
    s
}

fn short_time(ts: i64) -> String {
    let Some(t) = Local.timestamp_opt(ts, 0).single() else { return String::new() };
    let now = Local::now();
    if t.date_naive() == now.date_naive() {
        t.format("%H:%M").to_string()
    } else if (now.date_naive() - t.date_naive()).num_days() < 7 && t <= now {
        t.format("%a").to_string()
    } else if t.year() == now.year() {
        t.format("%-d %b").to_string()
    } else {
        t.format("%d.%m.%y").to_string()
    }
}

fn day_label(ts: i64) -> String {
    let Some(t) = Local.timestamp_opt(ts, 0).single() else { return String::new() };
    let today = Local::now().date_naive();
    let d = t.date_naive();
    if d == today {
        "Today".into()
    } else if Some(d) == today.pred_opt() {
        "Yesterday".into()
    } else if d.year() == today.year() {
        t.format("%A, %-d %B").to_string()
    } else {
        t.format("%A, %-d %B %Y").to_string()
    }
}

fn human_size(n: usize) -> String {
    match n {
        0..=1023 => format!("{n} B"),
        1024..=1_048_575 => format!("{:.0} KB", n as f64 / 1024.0),
        _ => format!("{:.1} MB", n as f64 / 1_048_576.0),
    }
}

/// Subject without reply/forward prefixes, for spotting topic changes.
fn base_subject(s: &str) -> String {
    let mut s = s.trim();
    loop {
        let lower = s.to_lowercase();
        let Some(p) = ["re:", "aw:", "fwd:", "fw:", "wg:", "tr:", "sv:", "antw:", "ответ:", "re :"].iter().find(|p| lower.starts_with(**p)) else { break };
        s = s[p.len()..].trim_start();
    }
    s.to_string()
}

fn reply_subject(s: &str) -> String {
    let b = base_subject(s);
    if b.is_empty() {
        String::new()
    } else {
        format!("Re: {b}")
    }
}

fn split_addresses(s: &str) -> Vec<String> {
    s.split([',', ';', ' ', '\n'])
        .map(|a| a.trim().trim_start_matches('<').trim_end_matches('>').to_lowercase())
        .filter(|a| a.contains('@') && a.len() > 2)
        .collect()
}

struct Draft {
    text: String,
    subject: String,
    subject_touched: bool,
    attachments: Vec<PathBuf>,
}

pub struct Core {
    ui: slint::Weak<AppWindow>,
    paths: Paths,
    settings: Settings,
    accounts: Vec<Account>,
    me: sync::Me,
    flags: HashMap<String, Flags>,
    tab: i32,
    creds: Arc<Creds>,
    shared: Arc<Shared>,
    workers: HashMap<String, Sender<Cmd>>,
    store: Store,
    names: HashMap<String, String>,

    filter: Option<usize>,
    search: String,
    peers: Vec<PeerRow>,
    /// Synthetic "write to <address>" entry at the top of the list.
    suggest: Option<String>,
    selected: Option<String>,
    limit: usize,
    mails: Vec<MailRow>,
    expanded: HashSet<i64>,
    requested: HashSet<i64>,
    messages: Rc<VecModel<MessageItem>>,
    from: usize,
    from_addr: String,
    drafts: HashMap<String, Draft>,
    attachments: Vec<PathBuf>,
    default_subject: String,
    sending: bool,
    last_peer_refresh: Instant,
    toast_until: Option<Instant>,
    favicons: Arc<Favicons>,
    /// Loaded website icons by domain; `None` while missing.
    icons: HashMap<String, Option<slint::Image>>,
    previews: HashMap<PreviewKey, Option<slint::Image>>,
    preview_jobs: PreviewJobs,
    previews_inflight: usize,
    /// In the "Yourself" conversation: the own address mail goes to.
    self_to: String,
    /// Showing made-up mail: no servers, nothing is sent.
    demo: bool,
    /// The Thunderbird profile on this computer, offered for import.
    tb_found: Option<PathBuf>,
    /// HTML renderings; `None` while rendering or when it failed.
    html_cache: HashMap<HtmlKey, Option<HtmlView>>,
    html_jobs: HtmlJobs,
    html_tx: Sender<(HtmlKey, f32)>,
    html_inflight: usize,
    /// Which rendering each visible mail shows, for link lookups.
    html_shown: HashMap<i64, HtmlKey>,
    /// Mails the user switched between formatted and text.
    format_override: HashMap<i64, bool>,
    /// Remote images allowed for these mails, or always for these senders.
    remote_once: HashSet<i64>,
    remote_senders: HashSet<String>,
}

thread_local! {
    static CORE: RefCell<Option<Rc<RefCell<Core>>>> = const { RefCell::new(None) };
}

pub(crate) fn with_core(f: impl FnOnce(&mut Core)) {
    CORE.with(|c| {
        if let Some(core) = c.borrow().as_ref() {
            if let Ok(mut core) = core.try_borrow_mut() {
                f(&mut core);
            }
        }
    });
}

/// Runs `f` on the UI thread with the core.
fn on_ui(f: impl FnOnce(&mut Core) + Send + 'static) {
    let _ = slint::invoke_from_event_loop(move || with_core(f));
}

impl Core {
    fn ui(&self) -> AppWindow {
        self.ui.upgrade().expect("window alive")
    }

    fn name_of(&self, addr: &str) -> Option<&String> {
        self.names.get(addr)
    }

    fn display(&self, addr: &str) -> String {
        self.name_of(addr).cloned().unwrap_or_else(|| addr.to_string())
    }

    fn first_name(&self, addr: &str) -> String {
        match self.name_of(addr) {
            Some(n) => n.split_whitespace().next().unwrap_or(n).to_string(),
            None => addr.split('@').next().unwrap_or(addr).to_string(),
        }
    }

    fn peer_title(&self, key: &str) -> String {
        if key.is_empty() {
            return "Yourself".into();
        }
        let addrs: Vec<&str> = key.split(',').collect();
        if addrs.len() == 1 {
            return self.display(addrs[0]);
        }
        let mut names: Vec<String> = addrs.iter().take(3).map(|a| self.first_name(a)).collect();
        if addrs.len() > 3 {
            names.push(format!("+{}", addrs.len() - 3));
        }
        names.join(", ")
    }

    fn peer_initials(&self, key: &str) -> String {
        let first = key.split(',').next().unwrap_or("");
        initials(&self.name_of(first).cloned().unwrap_or_else(|| first.split('@').next().unwrap_or("").to_string()))
    }

    fn peer_matches(&self, key: &str, q: &str) -> bool {
        (key.is_empty() && self.me.read().unwrap().iter().any(|a| a.contains(q))) || key.contains(q) || key.split(',').any(|a| self.name_of(a).is_some_and(|n| n.to_lowercase().contains(q))) || self.peer_title(key).to_lowercase().contains(q)
    }

    fn refresh_names(&mut self) {
        self.names = self.store.names();
    }

    fn account_index_for(&self, addr: &str) -> Option<usize> {
        self.accounts.iter().position(|a| a.addresses().any(|x| x.eq_ignore_ascii_case(addr)))
    }

    // ---------------------------------------------------------------- sidebar

    /// Short name of one of the own addresses, as on the account chips ("work", "gmail"); the
    /// local part when two own addresses share a domain.
    fn addr_label(&self, addr: &str) -> String {
        let domain_label = |a: &str| a.rsplit('@').next().unwrap_or(a).split('.').next().unwrap_or(a).to_lowercase();
        let label = domain_label(addr);
        let shared = self.own_addresses().iter().filter(|(_, a)| domain_label(a) == label).count() > 1;
        if shared { addr.split('@').next().unwrap_or(addr).to_string() } else { label }
    }

    fn addr_color(&self, addr: &str) -> slint::Color {
        match self.account_index_for(addr) {
            Some(i) => hue(&self.accounts[i].id),
            None => hue(addr),
        }
    }

    fn account_tags(&self, addrs: &[String]) -> Vec<AccountTag> {
        addrs.iter().map(|a| AccountTag { label: self.addr_label(a).into(), color: self.addr_color(a), addr: a.as_str().into() }).collect()
    }

    /// All own addresses with the account each belongs to.
    fn own_addresses(&self) -> Vec<(usize, String)> {
        self.accounts.iter().enumerate().flat_map(|(i, a)| a.addresses().map(move |x| (i, x.clone()))).collect()
    }

    pub(crate) fn refresh_peers(&mut self) {
        self.last_peer_refresh = Instant::now();
        let account = self.filter.and_then(|i| self.accounts.get(i)).map(|a| a.id.clone());
        let mut rows = self.store.peers(account.as_deref());
        // "Yourself" is always there, even before the first note.
        if !self.accounts.is_empty() && !rows.iter().any(|r| r.peer.is_empty()) {
            rows.push(PeerRow { peer: String::new(), last_date: 0, snippet: "Notes and files for yourself".into(), outgoing: false, unread: 0, junk: false, me_addrs: Vec::new() });
        }
        let flag = |k: &str| self.flags.get(k).copied().unwrap_or_default();
        let tab_of = |r: &PeerRow| tab_for(flag(&r.peer), r.junk);
        let (mut main_unread, mut muted_unread, mut muted_count, mut ignored_count, mut spam_count) = (0i64, 0i64, 0i32, 0i32, 0i32);
        for r in &rows {
            match tab_of(r) {
                0 => main_unread += r.unread,
                1 => {
                    muted_unread += r.unread;
                    muted_count += 1;
                }
                2 => ignored_count += 1,
                _ => spam_count += 1,
            }
        }
        let q = self.search.trim().to_lowercase();
        self.suggest = None;
        if q.is_empty() {
            rows.retain(|r| tab_of(r) == self.tab);
        } else {
            // Search looks through every tab.
            let text_hits = if q.chars().count() >= 3 { self.store.peers_matching(&q) } else { HashSet::new() };
            rows.retain(|r| self.peer_matches(&r.peer, &q) || text_hits.contains(&r.peer));
            let addrs = split_addresses(&q);
            if !addrs.is_empty() {
                let key = self.key_for(&addrs);
                if !rows.iter().any(|r| r.peer == key) {
                    self.suggest = Some(key);
                }
            }
        }
        // Favorites are pinned on top; both parts stay newest first.
        rows.sort_by_key(|r| !flag(&r.peer).favorite);
        // "Yourself" is pinned to the very top, above the favorites.
        if q.is_empty() {
            if let Some(i) = rows.iter().position(|r| r.peer.is_empty()) {
                let row = rows.remove(i);
                rows.insert(0, row);
            }
        }
        let has_favorites = q.is_empty() && rows.iter().any(|r| !r.peer.is_empty() && flag(&r.peer).favorite);
        let mut items: Vec<PeerItem> = Vec::with_capacity(rows.len() + 1);
        if let Some(key) = &self.suggest {
            items.push(PeerItem {
                key: key.into(),
                title: format!("Write to {}", key.replace(',', ", ")).into(),
                snippet: "Start a new conversation".into(),
                is_new: true,
                ..Default::default()
            });
        }
        let mut prev_fav = None;
        for r in &rows {
            let f = flag(&r.peer);
            let pinned = q.is_empty() && r.peer.is_empty();
            let section = if pinned {
                ""
            } else if has_favorites && prev_fav != Some(f.favorite) {
                if f.favorite { "Favorites" } else { "Chats" }
            } else {
                ""
            };
            if !pinned {
                prev_fav = Some(f.favorite);
            }
            let icon = icon_for(&mut self.icons, &self.favicons, self.settings.favicons, &r.peer);
            items.push(PeerItem {
                key: r.peer.as_str().into(),
                title: self.peer_title(&r.peer).into(),
                snippet: r.snippet.as_str().into(),
                time: if r.last_date > 0 { short_time(r.last_date) } else { String::new() }.into(),
                unread: r.unread as i32,
                initials: self.peer_initials(&r.peer).into(),
                hue: hue(&r.peer),
                group: r.peer.contains(','),
                outgoing: r.outgoing,
                is_new: false,
                favorite: f.favorite,
                muted: f.muted,
                ignored: f.ignored,
                section: section.into(),
                has_icon: icon.is_some(),
                icon: icon.unwrap_or_default(),
                me: r.peer.is_empty(),
                spam: tab_of(r) == 3,
                // Which of your addresses this contact writes to. For "Yourself" every
                // address is involved, so that says nothing.
                accounts: ModelRc::new(VecModel::from(if r.peer.is_empty() { Vec::new() } else { self.account_tags(&r.me_addrs[..r.me_addrs.len().min(2)]) })),
            });
        }
        self.peers = rows;
        let ui = self.ui();
        let app = ui.global::<App>();
        app.set_peers(ModelRc::new(VecModel::from(items)));
        app.set_total_unread(main_unread as i32);
        app.set_main_unread(main_unread as i32);
        app.set_muted_unread(muted_unread as i32);
        app.set_muted_count(muted_count);
        app.set_ignored_count(ignored_count);
        app.set_spam_count(spam_count);
        app.set_tab(self.tab);
        self.sync_selection_index();
    }

    fn sync_selection_index(&self) {
        let offset = self.suggest.is_some() as usize;
        let idx = self.selected.as_ref().and_then(|k| self.peers.iter().position(|p| &p.peer == k)).map(|i| (i + offset) as i32).unwrap_or(-1);
        self.ui().global::<App>().set_selected(idx);
    }

    fn key_for(&self, addrs: &[String]) -> String {
        let me = self.me.read().unwrap();
        let mut v: Vec<String> = addrs.iter().map(|a| a.to_lowercase()).filter(|a| !me.contains(a)).collect();
        v.sort();
        v.dedup();
        v.join(",")
    }

    fn refresh_status(&self) {
        let statuses = self.shared.status.lock().unwrap().clone();
        let mut busy = false;
        let mut text = String::new();
        let items: Vec<AccountItem> = self
            .accounts
            .iter()
            .map(|a| {
                let st = statuses.iter().find(|(id, _)| *id == a.id).map(|(_, s)| s.clone()).unwrap_or_default();
                busy |= st.busy;
                if text.is_empty() && st.busy && !st.message.is_empty() {
                    text = format!("{}: {}", a.short_label(), st.message);
                }
                if st.needs_login && text.is_empty() {
                    text = format!("{}: sign-in needed (Settings)", a.short_label());
                }
                AccountItem {
                    id: a.id.as_str().into(),
                    email: a.email.as_str().into(),
                    label: a.short_label().into(),
                    hue: hue(&a.id),
                    online: st.online,
                    busy: st.busy,
                    needs_login: st.needs_login,
                    oauth: matches!(a.auth, Auth::OAuth2(_)),
                    message: st.message.as_str().into(),
                }
            })
            .collect();
        let ui = self.ui();
        let app = ui.global::<App>();
        app.set_accounts(ModelRc::new(VecModel::from(items)));
        app.set_syncing(busy);
        app.set_sync_text(text.into());
    }

    // ---------------------------------------------------------- conversation

    fn save_draft(&mut self) {
        let Some(key) = self.selected.clone() else { return };
        let ui = self.ui();
        let app = ui.global::<App>();
        let text = app.get_draft_text().to_string();
        let subject = app.get_draft_subject().to_string();
        let subject_touched = subject != self.default_subject;
        if text.is_empty() && !subject_touched && self.attachments.is_empty() {
            self.drafts.remove(&key);
        } else {
            self.drafts.insert(key, Draft { text, subject, subject_touched, attachments: std::mem::take(&mut self.attachments) });
        }
    }

    pub(crate) fn open_peer(&mut self, key: String) {
        self.save_draft();
        let ui = self.ui();
        let app = ui.global::<App>();
        app.set_new_chat(false);
        app.set_people_open(false);
        self.selected = Some(key.clone());
        self.limit = PAGE;
        self.expanded.clear();
        self.load_conversation(true);

        // Read receipts back to the servers.
        let seen = self.store.mark_peer_seen(&key);
        self.flag(seen, true);

        // Defaults for the reply.
        let last = self.mails.iter().rev().find(|m| !m.outgoing).or(self.mails.last()).cloned();
        let (from, from_addr) = match &last {
            Some(m) => {
                let i = self.account_index_for(&m.me_addr).or_else(|| self.accounts.iter().position(|a| a.id == m.account)).unwrap_or(0);
                let addr = if self.accounts.get(i).is_some_and(|a| a.addresses().any(|x| x.eq_ignore_ascii_case(&m.me_addr))) { m.me_addr.clone() } else { self.accounts.get(i).map(|a| a.email.clone()).unwrap_or_default() };
                (i, addr)
            }
            None => (self.from.min(self.accounts.len().saturating_sub(1)), self.accounts.get(self.from).map(|a| a.email.clone()).unwrap_or_default()),
        };
        self.from = from;
        self.from_addr = from_addr;
        if key.is_empty() {
            // Notes go back to the address they went to last time.
            let me = self.me.read().unwrap().clone();
            self.self_to = last.and_then(|m| m.recipients.iter().map(|p| p.addr.to_lowercase()).find(|a| me.contains(a))).unwrap_or_else(|| self.from_addr.clone());
        }
        self.default_subject = self.reply_target().map(|m| reply_subject(&m.subject)).unwrap_or_default();
        match self.drafts.remove(&key) {
            Some(d) => {
                app.set_draft_text(d.text.into());
                app.set_draft_subject(if d.subject_touched { d.subject } else { self.default_subject.clone() }.into());
                self.attachments = d.attachments;
            }
            None => {
                app.set_draft_text(SharedString::new());
                app.set_draft_subject(self.default_subject.as_str().into());
                self.attachments.clear();
            }
        }
        self.update_composer();
        self.refresh_peers();
        ui.invoke_focus_composer();
    }

    fn update_composer(&self) {
        let ui = self.ui();
        let app = ui.global::<App>();
        app.set_draft_from(self.from_addr.as_str().into());
        let own: Vec<SharedString> = self.own_addresses().into_iter().map(|(_, a)| a.into()).collect();
        app.set_draft_from_choosable(own.len() > 1);
        app.set_own_addresses(ModelRc::new(VecModel::from(own)));
        app.set_draft_self(self.selected.as_deref() == Some("") && !app.get_new_chat());
        app.set_draft_to(self.self_to.as_str().into());
        let names: Vec<SharedString> = self.attachments.iter().map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default().into()).collect();
        app.set_draft_attachments(ModelRc::new(VecModel::from(names)));
    }

    fn load_conversation(&mut self, scroll: bool) {
        let Some(key) = self.selected.clone() else { return };
        let (mails, total) = self.store.conversation(&key, self.limit);
        let changed_tail = self.mails.last().map(|m| m.id) != mails.last().map(|m| m.id);
        self.mails = mails;

        // Bodies we don't have yet, grouped by the account that can fetch them.
        let mut wanted: HashMap<String, Vec<i64>> = HashMap::new();
        for m in self.mails.iter().filter(|m| !m.has_body && !self.requested.contains(&m.id)) {
            if let Some((account, _, _)) = self.store.locations(m.id).into_iter().next() {
                wanted.entry(account).or_default().push(m.id);
            }
        }
        for (account, ids) in wanted {
            self.requested.extend(ids.iter().copied());
            if let Some(w) = self.workers.get(&account) {
                let _ = w.send(Cmd::Bodies(ids));
            }
        }

        let ui = self.ui();
        let app = ui.global::<App>();
        let addrs: Vec<&str> = if key.is_empty() { Vec::new() } else { key.split(',').collect() };
        let group = addrs.len() > 1;
        app.set_has_chat(true);
        app.set_chat_title(self.peer_title(&key).into());
        app.set_chat_initials(self.peer_initials(&key).into());
        app.set_chat_hue(hue(&key));
        app.set_chat_group(group);
        app.set_chat_me(key.is_empty());
        let icon = icon_for(&mut self.icons, &self.favicons, self.settings.favicons, &key);
        app.set_chat_has_icon(icon.is_some());
        app.set_chat_icon(icon.unwrap_or_default());
        let f = self.flags.get(&key).copied().unwrap_or_default();
        app.set_chat_favorite(f.favorite);
        app.set_chat_muted(f.muted);
        app.set_chat_ignored(f.ignored);
        let junk = !self.mails.is_empty() && self.mails.iter().filter(|m| m.peer == key).all(|m| m.junk);
        app.set_chat_spam(tab_for(f, junk) == 3);
        let mut used: Vec<String> = Vec::new();
        for m in self.mails.iter().rev() {
            if !used.contains(&m.me_addr) {
                used.push(m.me_addr.clone());
            }
        }
        app.set_chat_accounts(ModelRc::new(VecModel::from(if key.is_empty() { Vec::new() } else { self.account_tags(&used) })));
        app.set_chat_subtitle(
            if key.is_empty() {
                "Notes and mail between your own addresses".to_string()
            } else if group {
                format!("{} people · {}", addrs.len(), addrs.join(", "))
            } else if self.name_of(addrs[0]).is_some() {
                addrs[0].to_string()
            } else {
                format!("{total} messages")
            }
            .into(),
        );
        let people: Vec<Participant> = addrs.iter().map(|a| Participant { name: self.name_of(a).cloned().unwrap_or_default().into(), addr: (*a).into() }).collect();
        app.set_chat_people(ModelRc::new(VecModel::from(people)));
        app.set_has_earlier((self.mails.len() as i64) < total);

        let me = self.me.read().unwrap().clone();
        let mut want_previews: Vec<PreviewKey> = Vec::new();
        let mut want_html: Vec<HtmlKey> = Vec::new();
        let mut shown_html: HashMap<i64, HtmlKey> = HashMap::new();
        let mut items = Vec::with_capacity(self.mails.len());
        let mut prev: Option<&MailRow> = None;
        for m in &self.mails {
            let day = day_label(m.date);
            let new_day = prev.is_none_or(|p| day_label(p.date) != day);
            let subject = base_subject(&m.subject);
            let new_subject = prev.is_none_or(|p| base_subject(&p.subject) != subject);
            let elsewhere = m.peer != key;
            let show_sender = (group || elsewhere) && !m.outgoing && (new_day || elsewhere || prev.is_none_or(|p| p.from.addr != m.from.addr || p.outgoing || p.peer != m.peer));
            let expanded = self.expanded.contains(&m.id);
            let formatted = m.has_html && m.has_body && self.format_override.get(&m.id).copied().unwrap_or(m.rich);
            let remote = self.remote_once.contains(&m.id) || self.remote_senders.contains(&m.from.addr);
            let html_key = (m.id, expanded, remote);
            let html = if formatted {
                match self.html_cache.get(&html_key) {
                    Some(v) => v.as_ref(),
                    None => {
                        want_html.push(html_key);
                        None
                    }
                }
            } else {
                None
            };
            if html.is_some() {
                shown_html.insert(m.id, html_key);
            }
            let mut text = if expanded { self.store.raw(m.id).map(|r| crate::parse::full_text(&r)).unwrap_or_else(|| m.text.clone()) } else { m.text.clone() };
            let long = text.len() > MAX_INLINE_TEXT;
            if long && !expanded {
                let mut cut = MAX_INLINE_TEXT;
                while !text.is_char_boundary(cut) {
                    cut -= 1;
                }
                text.truncate(cut);
                text.push_str(" …");
            }
            let atts: Vec<AttachmentItem> = m
                .attachments
                .iter()
                .map(|a| {
                    let preview = if m.has_body && previewable(a) {
                        match self.previews.get(&(m.id, a.index)) {
                            Some(p) => p.clone(),
                            None => {
                                want_previews.push((m.id, a.index));
                                None
                            }
                        }
                    } else {
                        None
                    };
                    AttachmentItem { name: a.name.as_str().into(), size: human_size(a.size).into(), index: a.index as i32, has_preview: preview.is_some(), preview: preview.unwrap_or_default() }
                })
                .collect();
            let via = if key.is_empty() {
                // Between own addresses, say which way it went.
                let to: Vec<&str> = m.recipients.iter().map(|p| p.addr.as_str()).filter(|a| me.contains(&a.to_lowercase())).collect();
                if to.is_empty() { String::new() } else { format!("{} → {}", m.from.addr, to.join(", ")) }
            } else if m.outgoing {
                format!("from {}", m.me_addr)
            } else {
                format!("to {}", m.me_addr)
            };
            items.push(MessageItem {
                id: m.id as i32,
                outgoing: m.outgoing,
                sender: if m.from.name.is_empty() { self.display(&m.from.addr) } else { self.name_of(&m.from.addr).cloned().unwrap_or_else(|| m.from.name.clone()) }.into(),
                hue: hue(&m.from.addr),
                show_sender,
                subject: if new_subject { m.subject.as_str() } else { "" }.into(),
                text: text.into(),
                can_expand: match html {
                    Some(v) => v.quoted || v.cut || expanded,
                    None => m.quoted || long || expanded,
                },
                html: html.map(|v| v.image.clone()).unwrap_or_default(),
                html_on: html.is_some(),
                html_height: html.map(|v| v.css_height).unwrap_or(0.0),
                html_blocked: html.is_some_and(|v| v.blocked) && !remote,
                html_formatted: formatted,
                expanded,
                time: Local.timestamp_opt(m.date, 0).single().map(|t| t.format("%H:%M").to_string()).unwrap_or_default().into(),
                full_time: Local.timestamp_opt(m.date, 0).single().map(|t| t.format("%A, %-d %B %Y, %H:%M").to_string()).unwrap_or_default().into(),
                day: if new_day { day } else { String::new() }.into(),
                via: via.into(),
                via_color: self.addr_color(&m.me_addr),
                junk: m.junk,
                loading: !m.has_body,
                has_html: m.has_html,
                unread: !m.seen,
                attachments: ModelRc::new(VecModel::from(atts)),
                group_label: if elsewhere { format!("Group · {}", self.peer_title(&m.peer)) } else { String::new() }.into(),
                group_key: if elsewhere { m.peer.as_str() } else { "" }.into(),
            });
            prev = Some(m);
        }
        // Update in place when the list is the same, so the view doesn't jump.
        let same = self.messages.row_count() == items.len() && items.iter().enumerate().all(|(i, it)| self.messages.row_data(i).is_some_and(|r| r.id == it.id));
        if same {
            for (i, it) in items.into_iter().enumerate() {
                if self.messages.row_data(i).as_ref() != Some(&it) {
                    self.messages.set_row_data(i, it);
                }
            }
        } else {
            self.messages.set_vec(items);
        }
        if scroll || changed_tail {
            app.set_scroll_token(app.get_scroll_token() + 1);
        }
        self.request_previews(want_previews);
        self.html_shown = shown_html;
        self.request_html(want_html);
    }

    fn request_html(&mut self, mut keys: Vec<HtmlKey>) {
        if keys.is_empty() {
            return;
        }
        // Make room, keeping what is on screen.
        if self.html_cache.len() + keys.len() > HTML_CACHE {
            let shown: HashSet<HtmlKey> = self.html_shown.values().copied().collect();
            self.html_cache.retain(|k, v| shown.contains(k) || v.is_none());
        }
        let scale = self.ui().window().scale_factor();
        // The newest mail is at the bottom, where the view starts.
        keys.reverse();
        for k in keys {
            self.html_cache.insert(k, None);
            self.html_inflight += 1;
            let _ = self.html_tx.send((k, scale));
        }
    }

    fn link_at(&self, id: i64, x: f32, y: f32) -> String {
        let Some(v) = self.html_shown.get(&id).and_then(|k| self.html_cache.get(k)).and_then(|v| v.as_ref()) else { return String::new() };
        v.links.iter().find(|l| x >= l.rect.0 && x < l.rect.0 + l.rect.2 && y >= l.rect.1 && y < l.rect.1 + l.rect.3).map(|l| l.url.clone()).unwrap_or_default()
    }

    fn request_previews(&mut self, keys: Vec<PreviewKey>) {
        if keys.is_empty() {
            return;
        }
        self.previews_inflight += keys.len();
        for k in &keys {
            // Placeholder so nothing is decoded twice.
            self.previews.insert(*k, None);
        }
        let db = self.paths.db();
        let jobs = self.preview_jobs.clone();
        std::thread::spawn(move || {
            let Ok(store) = Store::open(&db) else { return };
            for (id, index) in keys {
                let pixels = store.raw(id).and_then(|r| crate::parse::attachment_bytes(&r, index)).and_then(|b| decode_preview(&b));
                jobs.lock().unwrap().push(((id, index), pixels));
            }
        });
    }

    fn flag(&self, locs: Vec<(String, String, u32)>, seen: bool) {
        let mut by_account: HashMap<String, Vec<(String, u32)>> = HashMap::new();
        for (account, folder, uid) in locs {
            by_account.entry(account).or_default().push((folder, uid));
        }
        for (account, locs) in by_account {
            if let Some(w) = self.workers.get(&account) {
                let _ = w.send(Cmd::Flag(locs, seen));
            }
        }
    }

    /// The mail a reply answers: the newest one that isn't an automatic reply.
    fn reply_target(&self) -> Option<&MailRow> {
        let auto = |m: &&MailRow| {
            let s = m.subject.to_lowercase();
            ["automatische antwort", "automatic reply", "autoreply", "auto-reply", "auto response", "out of office", "abwesenheit", "réponse automatique", "undeliverable", "unzustellbar", "delivery status notification"].iter().any(|p| s.contains(p))
        };
        self.mails.iter().rev().find(|m| !auto(m)).or(self.mails.last())
    }

    fn find_mail(&self, id: i32) -> Option<&MailRow> {
        self.mails.iter().find(|m| m.id == id as i64)
    }

    // ------------------------------------------------------------- toasts

    fn toast(&mut self, text: impl Into<String>, error: bool) {
        let ui = self.ui();
        let app = ui.global::<App>();
        app.set_toast(text.into().into());
        app.set_toast_error(error);
        self.toast_until = Some(Instant::now() + Duration::from_secs(if error { 8 } else { 4 }));
    }

    // -------------------------------------------------------------- sending

    fn send(&mut self) {
        if self.sending {
            return;
        }
        let ui = self.ui();
        let app = ui.global::<App>();
        if app.get_new_chat() {
            self.confirm_new_chat();
            if app.get_new_chat() {
                return;
            }
        }
        let Some(key) = self.selected.clone() else { return };
        let text = app.get_draft_text().to_string();
        if text.trim().is_empty() && self.attachments.is_empty() {
            return;
        }
        let Some(account) = self.accounts.get(self.from).cloned() else {
            self.toast("No account to send from", true);
            return;
        };
        let to: Vec<Person> = if key.is_empty() {
            let addr = if self.self_to.is_empty() { self.from_addr.clone() } else { self.self_to.clone() };
            let name = self.account_index_for(&addr).and_then(|i| self.accounts.get(i)).map(|a| a.name.clone()).unwrap_or_default();
            vec![Person { name, addr }]
        } else {
            key.split(',').map(|a| Person { name: self.name_of(a).cloned().unwrap_or_default(), addr: a.to_string() }).collect()
        };
        if self.demo {
            self.toast("Demo mode: nothing is sent", false);
            return;
        }
        let subject = app.get_draft_subject().to_string();
        let last = self.reply_target();
        let threaded = last.is_some() && subject == self.default_subject;
        let (in_reply_to, references) = match last.filter(|_| threaded) {
            Some(m) => {
                let mut refs: Vec<String> = m.refs.split_whitespace().map(str::to_string).collect();
                if refs.is_empty() && !m.in_reply_to.is_empty() {
                    refs.push(m.in_reply_to.clone());
                }
                refs.push(m.msgid.clone());
                (Some(m.msgid.clone()), refs)
            }
            None => (None, Vec::new()),
        };
        let draft = crate::send::Draft {
            from: Person { name: account.name.clone(), addr: self.from_addr.clone() },
            account,
            to,
            subject,
            text,
            in_reply_to,
            references,
            attachments: self.attachments.clone(),
        };
        self.sending = true;
        app.set_sending(true);
        let creds = self.creds.clone();
        let db = self.paths.db();
        let worker = self.workers.get(&draft.account.id).cloned();
        let shared = self.shared.clone();
        std::thread::spawn(move || {
            let result = crate::send::send(&draft, &creds).and_then(|(raw, header)| {
                let store = Store::open(&db).map_err(|e| e.to_string())?;
                let placement = Placement { peer: key.clone(), outgoing: true, me_addr: draft.from.addr.to_lowercase() };
                let body = crate::parse::body(&raw);
                store.add_sent(&draft.account.id, &header, &placement, &raw, &body).map_err(|e| e.to_string())?;
                if draft.account.save_sent {
                    if let Some(w) = worker {
                        let _ = w.send(Cmd::AppendSent(raw));
                    }
                }
                shared.dirty.store(true, Ordering::Relaxed);
                Ok(())
            });
            on_ui(move |core| {
                core.sending = false;
                let ui = core.ui();
        let app = ui.global::<App>();
                app.set_sending(false);
                match result {
                    Ok(()) => {
                        if core.selected.as_deref() == Some(key.as_str()) {
                            app.set_draft_text(SharedString::new());
                            core.attachments.clear();
                            core.update_composer();
                            core.load_conversation(true);
                            core.default_subject = core.reply_target().map(|m| reply_subject(&m.subject)).unwrap_or_default();
                            app.set_draft_subject(core.default_subject.as_str().into());
                        } else {
                            core.drafts.remove(&key);
                        }
                        core.refresh_peers();
                    }
                    Err(e) => core.toast(format!("Not sent: {e}"), true),
                }
            });
        });
    }

    fn confirm_new_chat(&mut self) {
        let ui = self.ui();
        let app = ui.global::<App>();
        let addrs = split_addresses(&app.get_new_chat_to());
        if addrs.is_empty() {
            self.toast("Enter at least one email address", true);
            return;
        }
        let key = self.key_for(&addrs);
        // Keep anything typed while choosing recipients.
        let text = app.get_draft_text();
        let subject = app.get_draft_subject();
        let attachments = std::mem::take(&mut self.attachments);
        self.selected = None;
        self.open_peer(key);
        if !text.is_empty() {
            app.set_draft_text(text);
        }
        if !subject.is_empty() {
            app.set_draft_subject(subject);
        }
        if !attachments.is_empty() {
            self.attachments = attachments;
            self.update_composer();
        }
    }

    // ---------------------------------------------------------- appearance

    pub(crate) fn forget_accounts_for_shot(&mut self) {
        self.accounts.clear();
        self.refresh_status();
    }

    pub(crate) fn set_tab(&mut self, tab: i32) {
        self.tab = tab;
        self.refresh_peers();
    }

    pub(crate) fn set_look(&mut self, theme: i32, accent: i32) {
        self.settings.theme = theme;
        self.settings.accent = accent;
        self.apply_appearance();
    }

    pub(crate) fn apply_appearance(&self) {
        let ui = self.ui();
        ui.global::<Theme>().set_accent_scheme(self.settings.accent);
        let scheme = match self.settings.theme {
            1 => slint::language::ColorScheme::Light,
            2 => slint::language::ColorScheme::Dark,
            _ => slint::language::ColorScheme::Unknown,
        };
        ui.global::<Palette>().set_color_scheme(scheme);
        let app = ui.global::<App>();
        app.set_accent(self.settings.accent);
        app.set_theme_mode(self.settings.theme);
        app.set_show_favicons(self.settings.favicons);
    }

    // ------------------------------------------------------ notifications

    fn notify_fresh(&mut self) {
        let fresh: Vec<i64> = std::mem::take(&mut *self.shared.fresh.lock().unwrap());
        if fresh.is_empty() || !(self.settings.notify_desktop || self.settings.notify_sound) {
            return;
        }
        let flag = |k: &str| self.flags.get(k).copied().unwrap_or_default();
        let mut items: Vec<(String, String, String)> = Vec::new();
        for id in fresh {
            let Some((peer, from_addr, from_name, preview, outgoing, seen)) = self.store.mail_brief(id) else { continue };
            if outgoing || seen || self.selected.as_deref() == Some(peer.as_str()) {
                continue;
            }
            let (chat, sender) = (flag(&peer), flag(&from_addr));
            let junk = self.store.is_junk(id);
            if chat.muted || chat.ignored || sender.ignored || chat.spam == 1 || (junk && chat.spam != -1) {
                continue;
            }
            let who = self.name_of(&from_addr).cloned().filter(|n| !n.is_empty()).unwrap_or(if from_name.is_empty() { from_addr.clone() } else { from_name });
            let title = if peer.contains(',') { format!("{who} in {}", self.peer_title(&peer)) } else { who };
            items.push((title, preview, peer));
        }
        if items.is_empty() {
            return;
        }
        if self.settings.notify_desktop {
            let open = |peer: String| -> Option<Box<dyn FnOnce() + Send>> {
                Some(Box::new(move || {
                    on_ui(move |c| {
                        c.open_peer(peer);
                        let _ = c.ui().show();
                    })
                }))
            };
            if items.len() <= 3 {
                for (title, body, peer) in &items {
                    crate::notify::show(&self.paths.data, title, body, open(peer.clone()));
                }
            } else {
                let mut names: Vec<&str> = items.iter().map(|(t, _, _)| t.as_str()).collect();
                names.dedup();
                crate::notify::show(&self.paths.data, &format!("{} new messages", items.len()), &names.join(", "), open(items[0].2.clone()));
            }
        }
        if self.settings.notify_sound {
            crate::notify::sound(&self.paths.data);
        }
    }

    // ------------------------------------------------------------- flags

    fn row_key(&self, index: i32) -> Option<String> {
        let offset = self.suggest.is_some() as i32;
        if index < offset {
            return None;
        }
        self.peers.get((index - offset) as usize).map(|p| p.peer.clone())
    }

    fn toggle_flag(&mut self, key: String, which: i32) {
        let f = self.flags.entry(key.clone()).or_default();
        let (flag, on, msg) = match which {
            0 => {
                f.favorite = !f.favorite;
                (Flag::Favorite, f.favorite, if f.favorite { "Pinned to the top" } else { "Removed from favorites" })
            }
            1 => {
                f.muted = !f.muted;
                (Flag::Muted, f.muted, if f.muted { "Muted: no notifications, moved to the Muted tab" } else { "Unmuted" })
            }
            2 => {
                f.ignored = !f.ignored;
                (Flag::Ignored, f.ignored, if f.ignored { "Ignored: moved to the Ignored tab" } else { "No longer ignored" })
            }
            _ => {
                let junk = self.peers.iter().find(|p| p.peer == key).is_some_and(|p| p.junk);
                let now_spam = tab_for(*f, junk) == 3;
                f.spam = if now_spam { -1 } else { 1 };
                if !now_spam {
                    f.muted = false;
                    f.ignored = false;
                }
                let value = f.spam;
                self.store.set_spam(&key, value);
                if !now_spam {
                    self.store.set_flag(&key, Flag::Muted, false);
                    self.store.set_flag(&key, Flag::Ignored, false);
                }
                self.toast(format!("{}: {}", self.peer_title(&key), if now_spam { "Not spam, back in Chats" } else { "Marked as spam: moved to the Spam tab" }), false);
                self.refresh_peers();
                if self.selected.as_deref() == Some(key.as_str()) {
                    self.load_conversation(false);
                }
                return;
            }
        };
        self.store.set_flag(&key, flag, on);
        self.toast(format!("{}: {msg}", self.peer_title(&key)), false);
        self.refresh_peers();
        if self.selected.as_deref() == Some(key.as_str()) {
            self.load_conversation(false);
        }
    }

    // ----------------------------------------------------------- accounts

    fn spawn_worker(&mut self, acct: &Account) {
        let w = sync::spawn(acct.clone(), self.me.clone(), self.creds.clone(), self.shared.clone(), &self.paths);
        self.workers.insert(acct.id.clone(), w);
    }

    /// Own addresses changed: re-sort existing mail into conversations in the background.
    fn me_changed(&mut self) {
        *self.me.write().unwrap() = sync::me_addresses(&self.accounts);
        let me = self.me.read().unwrap().clone();
        let db = self.paths.db();
        let shared = self.shared.clone();
        std::thread::spawn(move || {
            if let Err(e) = Store::open(&db).and_then(|s| s.replace_me(&me)) {
                eprintln!("regroup: {e}");
            }
            shared.dirty.store(true, Ordering::Relaxed);
        });
    }

    fn save_accounts(&self) {
        if let Err(e) = save_json(&self.paths.accounts(), &self.accounts, false) {
            eprintln!("cannot save accounts: {e}");
        }
    }

    fn add_account(&mut self, acct: Account) {
        if let Some(existing) = self.accounts.iter_mut().find(|a| a.id == acct.id) {
            *existing = acct.clone();
            if let Some(w) = self.workers.get(&acct.id) {
                let _ = w.send(Cmd::Reconnect);
            }
        } else {
            self.accounts.push(acct.clone());
            self.spawn_worker(&acct);
        }
        self.save_accounts();
        let known = self.me.read().unwrap().clone();
        if acct.addresses().any(|a| !known.contains(&a.to_lowercase())) {
            self.me_changed();
        }
        self.refresh_status();
    }

    fn remove_account(&mut self, id: &str) {
        let Some(i) = self.accounts.iter().position(|a| a.id == id) else { return };
        let acct = self.accounts.remove(i);
        if let Some(w) = self.workers.remove(id) {
            let _ = w.send(Cmd::Stop);
        }
        self.creds.forget(id);
        self.shared.status.lock().unwrap().retain(|(a, _)| a != id);
        self.save_accounts();
        self.filter = None;
        self.ui().global::<App>().set_account_filter(-1);
        self.from = 0;
        let db = self.paths.db();
        let shared = self.shared.clone();
        let account = acct.id.clone();
        std::thread::spawn(move || {
            if let Ok(s) = Store::open(&db) {
                s.remove_account(&account);
            }
            shared.dirty.store(true, Ordering::Relaxed);
        });
        self.me_changed();
        self.refresh_status();
        self.toast(format!("Removed {}", acct.email), false);
    }

    fn form_account(&self) -> Result<(Account, String), String> {
        let ui = self.ui();
        let app = ui.global::<App>();
        let email = app.get_add_email().trim().to_lowercase();
        if !email.contains('@') {
            return Err("Enter a valid email address".into());
        }
        let sec = |i: i32| match i {
            1 => Security::StartTls,
            2 => Security::Plain,
            _ => Security::Tls,
        };
        let port = |s: SharedString, what: &str| s.trim().parse::<u16>().map_err(|_| format!("{what} port must be a number"));
        let username = app.get_add_username().trim().to_string();
        let username = if username.is_empty() { email.clone() } else { username };
        let imap = Server { host: app.get_add_imap_host().trim().to_string(), port: port(app.get_add_imap_port(), "IMAP")?, security: sec(app.get_add_imap_sec()), username: username.clone() };
        let smtp = Server { host: app.get_add_smtp_host().trim().to_string(), port: port(app.get_add_smtp_port(), "SMTP")?, security: sec(app.get_add_smtp_sec()), username };
        if imap.host.is_empty() || smtp.host.is_empty() {
            return Err("Enter both server names".into());
        }
        let auth = match app.get_add_oauth() {
            1 => Auth::OAuth2(Issuer::Google),
            2 => Auth::OAuth2(Issuer::Microsoft),
            _ => Auth::Password,
        };
        let password = app.get_add_password().to_string();
        if auth == Auth::Password && password.is_empty() {
            return Err("Enter your password".into());
        }
        let save_sent = crate::config::issuer_for_host(&imap.host).is_none();
        let name = app.get_add_name().trim().to_string();
        Ok((Account { id: email.clone(), email, name, aliases: Vec::new(), imap, smtp, auth, save_sent }, password))
    }

    fn add_status(&self, text: impl Into<String>, error: bool, busy: bool) {
        let ui = self.ui();
        let app = ui.global::<App>();
        app.set_add_status(text.into().into());
        app.set_add_error(error);
        app.set_add_busy(busy);
    }

    fn tick(&mut self) {
        self.notify_fresh();
        if self.favicons.updated.swap(false, Ordering::Relaxed) {
            self.icons.retain(|_, v| v.is_some());
            self.refresh_peers();
            if self.selected.is_some() {
                self.load_conversation(false);
            }
        }
        let rendered: Vec<_> = std::mem::take(&mut *self.html_jobs.lock().unwrap());
        if !rendered.is_empty() {
            self.html_inflight = self.html_inflight.saturating_sub(rendered.len());
            for (k, r) in rendered {
                match r {
                    Some(r) => {
                        let view = HtmlView { image: slint::Image::from_rgba8(r.pixels), links: r.links, css_height: r.css_height, blocked: r.blocked, quoted: r.quoted, cut: r.cut };
                        self.html_cache.insert(k, Some(view));
                    }
                    None => {
                        // Couldn't render: show this mail as text.
                        self.html_cache.remove(&k);
                        self.format_override.insert(k.0, false);
                    }
                }
            }
            self.load_conversation(false);
        }
        let done: Vec<_> = std::mem::take(&mut *self.preview_jobs.lock().unwrap());
        if !done.is_empty() {
            self.previews_inflight = self.previews_inflight.saturating_sub(done.len());
            for (k, pixels) in done {
                self.previews.insert(k, pixels.map(slint::Image::from_rgba8));
            }
            self.load_conversation(false);
        }
        if let Some(t) = self.toast_until {
            if Instant::now() > t {
                self.toast_until = None;
                self.ui().global::<App>().set_toast(SharedString::new());
            }
        }
        let dirty = self.shared.dirty.load(Ordering::Relaxed);
        if dirty && self.last_peer_refresh.elapsed() > Duration::from_millis(900) {
            self.shared.dirty.store(false, Ordering::Relaxed);
            self.refresh_names();
            self.refresh_status();
            self.refresh_peers();
            if self.selected.is_some() && !self.ui().global::<App>().get_new_chat() {
                self.load_conversation(false);
                // New mail in the open conversation is read right away.
                if let Some(key) = self.selected.clone() {
                    if self.mails.iter().any(|m| !m.seen) {
                        let seen = self.store.mark_peer_seen(&key);
                        self.flag(seen, true);
                    }
                }
            }
        }
    }
}

/// `thundergoose --check`: logs in to every server without changing anything.
pub fn check() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let paths = Paths::new();
    let settings: Settings = load_json(&paths.settings()).unwrap_or_default();
    let (accounts, secrets, _, linked) = load_accounts(&paths, &settings);
    if accounts.is_empty() {
        println!("No accounts yet. Start Thundergoose to add one or to import them from Thunderbird.");
    }
    let creds = Creds::new(&paths, secrets, linked);
    for a in &accounts {
        let imap = match crate::imapc::connect(a, &creds) {
            Ok(mut s) => {
                let _ = s.logout();
                "ok".to_string()
            }
            Err(e) => e.to_string(),
        };
        let smtp = match crate::send::check(a, &creds) {
            Ok(()) => "ok".to_string(),
            Err(e) => e,
        };
        println!("{:<36} IMAP {imap:<6} SMTP {smtp}", a.email);
    }
}

/// Accounts and secrets as saved, the Thunderbird profile found on this computer, and that
/// profile again if the user chose to stay linked to it. Nothing is imported by itself.
fn load_accounts(paths: &Paths, settings: &Settings) -> (Vec<Account>, Secrets, Option<PathBuf>, Option<PathBuf>) {
    let found = settings.thunderbird_profile.clone().filter(|p| p.join("prefs.js").exists()).or_else(crate::tb::find_profile);
    let accounts = load_json(&paths.accounts()).unwrap_or_default();
    let secrets = load_json(&paths.secrets()).unwrap_or_default();
    let linked = found.clone().filter(|_| settings.tb_linked());
    (accounts, secrets, found, linked)
}

pub fn run(demo: bool) -> Result<(), slint::PlatformError> {
    let (ui, _timers) = start(demo)?;
    ui.run()?;
    with_core(|c| c.save_draft());
    Ok(())
}

/// Builds the window and everything behind it. With `demo`, the data directory holds made-up
/// mail: Thunderbird is left alone and no server is contacted.
pub fn start(demo: bool) -> Result<(AppWindow, Vec<slint::Timer>), slint::PlatformError> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let paths = Paths::new();
    let settings: Settings = load_json(&paths.settings()).unwrap_or_default();
    let (accounts, secrets, profile, linked) = if demo {
        (load_json(&paths.accounts()).unwrap_or_default(), Secrets::default(), None, None)
    } else {
        load_accounts(&paths, &settings)
    };
    let store = Store::open(&paths.db()).map_err(|e| slint::PlatformError::Other(format!("cannot open {}: {e}", paths.db().display())))?;
    if let Some(p) = &linked {
        store.import_names(&crate::tb::address_book(p));
    }
    if store.parse_version() < crate::store::PARSE_VERSION {
        let db = paths.db();
        std::thread::spawn(move || {
            if let Err(e) = Store::open(&db).and_then(|s| s.reparse_all()) {
                eprintln!("reparse: {e}");
            }
        });
    }
    let me: sync::Me = Arc::new(std::sync::RwLock::new(sync::me_addresses(&accounts)));
    let flags = store.flags();
    let creds = Arc::new(Creds::new(&paths, secrets, linked.clone()));
    let shared = Arc::new(Shared::new());
    let workers: HashMap<String, Sender<Cmd>> = if demo {
        for a in &accounts {
            shared.status.lock().unwrap().push((a.id.clone(), sync::Status { online: true, ..Default::default() }));
        }
        HashMap::new()
    } else {
        accounts.iter().map(|a| (a.id.clone(), sync::spawn(a.clone(), me.clone(), creds.clone(), shared.clone(), &paths))).collect()
    };
    let favicons = Favicons::new(&paths.data);
    let html_jobs: HtmlJobs = Arc::default();
    let html_tx = spawn_html_renderer(paths.db(), html_jobs.clone());
    let remote_senders = store.remote_image_senders();

    let ui = AppWindow::new()?;
    let app = ui.global::<App>();
    app.set_version(env!("CARGO_PKG_VERSION").into());
    app.set_notify_desktop(settings.notify_desktop);
    app.set_notify_sound(settings.notify_sound);
    app.set_profile_path(profile.as_ref().map(|p| p.display().to_string()).unwrap_or_default().into());
    app.set_tb_linked(linked.is_some());
    let messages = Rc::new(VecModel::<MessageItem>::default());
    app.set_messages(ModelRc::from(messages.clone()));

    let core = Rc::new(RefCell::new(Core {
        ui: ui.as_weak(),
        paths,
        settings,
        from_addr: accounts.first().map(|a| a.email.clone()).unwrap_or_default(),
        accounts,
        me,
        flags,
        tab: 0,
        creds,
        shared,
        workers,
        store,
        names: HashMap::new(),
        filter: None,
        search: String::new(),
        peers: Vec::new(),
        suggest: None,
        selected: None,
        limit: PAGE,
        mails: Vec::new(),
        expanded: HashSet::new(),
        requested: HashSet::new(),
        messages,
        from: 0,
        drafts: HashMap::new(),
        attachments: Vec::new(),
        default_subject: String::new(),
        sending: false,
        last_peer_refresh: Instant::now() - Duration::from_secs(10),
        toast_until: None,
        favicons,
        icons: HashMap::new(),
        previews: HashMap::new(),
        preview_jobs: Arc::default(),
        previews_inflight: 0,
        self_to: String::new(),
        demo,
        tb_found: profile,
        html_cache: HashMap::new(),
        html_jobs,
        html_tx,
        html_inflight: 0,
        html_shown: HashMap::new(),
        format_override: HashMap::new(),
        remote_once: HashSet::new(),
        remote_senders,
    }));
    CORE.with(|c| *c.borrow_mut() = Some(core.clone()));

    {
        let mut c = core.borrow_mut();
        c.apply_appearance();
        c.refresh_names();
        c.refresh_status();
        c.refresh_peers();
        if c.accounts.is_empty() {
            // First start: offer the Thunderbird import if there is a profile, else go straight
            // to adding an account.
            let ui = c.ui();
            let app = ui.global::<App>();
            app.set_settings_open(true);
            app.set_add_open(c.tb_found.is_none());
        }
    }

    // Development aid: open a conversation on startup.
    match std::env::var("THUNDERGOOSE_OPEN").as_deref() {
        Ok(":settings") => app.set_settings_open(true),
        Ok(key) => core.borrow_mut().open_peer(key.to_string()),
        Err(_) => {}
    }

    app.on_select_peer(|i| {
        with_core(|c| {
            let offset = c.suggest.is_some() as i32;
            let key = if i < offset { c.suggest.clone() } else { c.peers.get((i - offset).max(0) as usize).map(|p| p.peer.clone()) };
            if let Some(key) = key {
                if c.suggest.as_ref() == Some(&key) {
                    c.search.clear();
                    c.ui().global::<App>().set_search(SharedString::new());
                }
                c.open_peer(key);
            }
        })
    });
    app.on_search_edited(|t| {
        with_core(|c| {
            c.search = t.to_string();
            c.refresh_peers();
        })
    });
    app.on_search_accepted(|| {
        with_core(|c| {
            let key = c.suggest.clone().or_else(|| c.peers.first().map(|p| p.peer.clone()));
            if let Some(key) = key {
                c.open_peer(key);
            }
        })
    });
    app.on_filter(|i| {
        with_core(|c| {
            c.filter = (i >= 0).then_some(i as usize);
            c.ui().global::<App>().set_account_filter(i);
            c.refresh_peers();
        })
    });
    app.on_load_earlier(|| {
        with_core(|c| {
            c.limit += PAGE * 2;
            c.load_conversation(false);
        })
    });
    app.on_toggle_message(|id| {
        with_core(|c| {
            let id = id as i64;
            if !c.expanded.remove(&id) {
                c.expanded.insert(id);
            }
            c.load_conversation(false);
        })
    });
    app.on_link_at(|id, x, y| {
        let mut url = String::new();
        with_core(|c| url = c.link_at(id as i64, x, y));
        url.into()
    });
    app.on_short_url(|url| {
        let s = url.as_str();
        if s.chars().count() > 90 { format!("{}…", s.chars().take(90).collect::<String>()).into() } else { url }
    });
    app.on_open_link(|url| {
        with_core(|c| {
            if let Some(addr) = url.strip_prefix("mailto:") {
                // Writing to someone stays in Thundergoose.
                let addr = addr.split('?').next().unwrap_or(addr).to_string();
                let key = c.key_for(&[addr]);
                c.open_peer(key);
            } else if let Err(e) = open::that_detached(url.as_str()) {
                c.toast(format!("Cannot open link: {e}"), true);
            }
        })
    });
    app.on_load_images(|id, always| {
        with_core(|c| {
            let id = id as i64;
            if always {
                if let Some(m) = c.find_mail(id as i32) {
                    let from = m.from.addr.clone();
                    c.store.set_remote_images(&from, true);
                    c.toast(format!("Images from {from} will always load"), false);
                    c.remote_senders.insert(from);
                }
            } else {
                c.remote_once.insert(id);
            }
            c.load_conversation(false);
        })
    });
    app.on_toggle_format(|id| {
        with_core(|c| {
            let id = id as i64;
            let Some(m) = c.find_mail(id as i32) else { return };
            let now = c.format_override.get(&id).copied().unwrap_or(m.rich);
            c.format_override.insert(id, !now);
            c.load_conversation(false);
        })
    });
    app.on_open_html(|id| {
        with_core(|c| {
            let Some(raw) = c.store.raw(id as i64) else { return };
            let Some(html) = crate::parse::html(&raw) else { return };
            // Block remote content and scripts: no tracking pixels, nothing runs.
            let csp = "<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; img-src data: cid:; style-src 'unsafe-inline'; font-src data:\"><meta charset=\"utf-8\">";
            let path = std::env::temp_dir().join(format!("thundergoose-{id}.html"));
            if std::fs::write(&path, format!("{csp}{html}")).is_ok() {
                let _ = open::that_detached(&path);
            }
        })
    });
    app.on_open_attachment(|id, index| {
        with_core(|c| {
            let Some(m) = c.find_mail(id) else { return };
            let Some(att) = m.attachments.iter().find(|a| a.index as i32 == index).cloned() else { return };
            let Some(bytes) = c.store.raw(id as i64).and_then(|r| crate::parse::attachment_bytes(&r, att.index)) else { return };
            let dir = std::env::temp_dir().join(format!("thundergoose-{}-{id}", std::process::id()));
            let _ = std::fs::create_dir_all(&dir);
            let safe: String = att.name.chars().map(|ch| if matches!(ch, '/' | '\\' | ':' | '\0') { '_' } else { ch }).collect();
            let path = dir.join(safe);
            match std::fs::write(&path, bytes) {
                Ok(()) => {
                    let _ = open::that_detached(&path);
                }
                Err(e) => c.toast(format!("Cannot open attachment: {e}"), true),
            }
        })
    });
    app.on_save_attachment(|id, index| {
        with_core(|c| {
            let Some(m) = c.find_mail(id) else { return };
            let Some(att) = m.attachments.iter().find(|a| a.index as i32 == index).cloned() else { return };
            let Some(bytes) = c.store.raw(id as i64).and_then(|r| crate::parse::attachment_bytes(&r, att.index)) else { return };
            if let Some(path) = rfd::FileDialog::new().set_file_name(&att.name).save_file() {
                match std::fs::write(&path, bytes) {
                    Ok(()) => c.toast(format!("Saved {}", path.display()), false),
                    Err(e) => c.toast(format!("Cannot save: {e}"), true),
                }
            }
        })
    });
    app.on_copy(|text| {
        with_core(|c| match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(text.to_string())) {
            Ok(()) => c.toast("Copied", false),
            Err(e) => c.toast(format!("Clipboard unavailable: {e}"), true),
        })
    });
    app.on_send(|| with_core(|c| c.send()));
    app.on_attach(|| {
        with_core(|c| {
            if let Some(files) = rfd::FileDialog::new().pick_files() {
                c.attachments.extend(files);
                c.update_composer();
            }
        })
    });
    app.on_remove_attachment(|i| {
        with_core(|c| {
            if (i as usize) < c.attachments.len() {
                c.attachments.remove(i as usize);
                c.update_composer();
            }
        })
    });
    app.on_choose_from(|i| {
        with_core(|c| {
            if let Some((acct, addr)) = c.own_addresses().get(i as usize).cloned() {
                c.from = acct;
                c.from_addr = addr;
                c.update_composer();
            }
        })
    });
    app.on_choose_to(|i| {
        with_core(|c| {
            let Some((acct, addr)) = c.own_addresses().get(i as usize).cloned() else { return };
            // A note sent from the address it goes to stays that way.
            if c.from_addr.eq_ignore_ascii_case(&c.self_to) {
                c.from = acct;
                c.from_addr = addr.clone();
            }
            c.self_to = addr;
            c.update_composer();
        })
    });
    app.on_start_new_chat(|| {
        with_core(|c| {
            c.save_draft();
            c.selected = None;
            c.mails.clear();
            c.messages.set_vec(Vec::new());
            c.attachments.clear();
            c.default_subject.clear();
            let ui = c.ui();
            let app = ui.global::<App>();
            app.set_has_chat(false);
            app.set_new_chat(true);
            app.set_new_chat_to(SharedString::new());
            app.set_draft_text(SharedString::new());
            app.set_draft_subject(SharedString::new());
            app.set_has_earlier(false);
            c.update_composer();
            c.sync_selection_index();
            ui.invoke_focus_to();
        })
    });
    app.on_new_chat_confirm(|| with_core(|c| c.confirm_new_chat()));
    app.on_cancel_new_chat(|| {
        with_core(|c| {
            let ui = c.ui();
        let app = ui.global::<App>();
            app.set_new_chat(false);
            app.set_has_chat(false);
        })
    });
    app.on_write_to(|addr| {
        with_core(|c| {
            let key = c.key_for(&[addr.to_string()]);
            c.open_peer(key);
        })
    });
    app.on_mark_unread(|| {
        with_core(|c| {
            let Some(key) = c.selected.clone() else { return };
            let locs = c.store.mark_peer_unseen(&key);
            c.flag(locs, false);
            c.selected = None;
            let ui = c.ui();
        let app = ui.global::<App>();
            app.set_has_chat(false);
            c.refresh_peers();
        })
    });
    app.on_sync_now(|| {
        with_core(|c| {
            for w in c.workers.values() {
                let _ = w.send(Cmd::Wake);
            }
            c.toast("Checking for new mail…", false);
        })
    });
    app.on_sign_in(|id| {
        with_core(|c| {
            let Some(acct) = c.accounts.iter().find(|a| a.id == id.as_str()).cloned() else { return };
            let creds = c.creds.clone();
            let worker = c.workers.get(&acct.id).cloned();
            c.toast(format!("Continue signing in to {} in your browser…", acct.email), false);
            std::thread::spawn(move || {
                let r = creds.sign_in(&acct);
                if r.is_ok() {
                    if let Some(w) = worker {
                        let _ = w.send(Cmd::Reconnect);
                    }
                }
                on_ui(move |c| match r {
                    Ok(()) => c.toast(format!("Signed in to {}", acct.email), false),
                    Err(e) => c.toast(format!("Sign-in failed: {e}"), true),
                });
            });
        })
    });
    app.on_set_password(|id, pw| {
        with_core(|c| {
            if pw.is_empty() {
                return;
            }
            c.creds.set("imap", &id, pw.to_string());
            c.creds.set("smtp", &id, pw.to_string());
            if let Some(w) = c.workers.get(id.as_str()) {
                let _ = w.send(Cmd::Reconnect);
            }
            c.toast("Password saved, reconnecting…", false);
        })
    });
    app.on_reimport(|| {
        with_core(|c| {
            let Some(p) = c.tb_found.clone() else { return };
            let password = c.ui().global::<App>().get_tb_password().to_string();
            match crate::tb::import(&p, &password) {
                Ok(imp) => {
                    for (k, v) in &imp.secrets.0 {
                        if let Some((kind, acct)) = k.split_once(':') {
                            c.creds.set(kind, acct, v.clone());
                        }
                    }
                    c.store.import_names(&crate::tb::address_book(&p));
                    c.refresh_names();
                    let n = imp.accounts.len();
                    for acct in imp.accounts {
                        c.add_account(acct);
                    }
                    c.ui().global::<App>().set_tb_password(SharedString::new());
                    // Importing links the profile: names and renewed sign-ins keep coming.
                    c.settings.thunderbird_profile = Some(p.clone());
                    c.settings.thunderbird_linked = Some(true);
                    let _ = save_json(&c.paths.settings(), &c.settings, false);
                    c.creds.set_tb_profile(Some(p.clone()));
                    c.ui().global::<App>().set_tb_linked(true);
                    let mut msg = format!("Imported {n} accounts from Thunderbird");
                    if !imp.warnings.is_empty() {
                        msg.push_str(&format!(" · {}", imp.warnings.join(" · ")));
                    }
                    c.toast(msg, !imp.warnings.is_empty());
                }
                Err(e) => c.toast(e, true),
            }
        })
    });
    app.on_tb_link_changed(|| {
        with_core(|c| {
            let on = c.ui().global::<App>().get_tb_linked();
            c.settings.thunderbird_linked = Some(on);
            let _ = save_json(&c.paths.settings(), &c.settings, false);
            let profile = c.tb_found.clone().filter(|_| on);
            if let Some(p) = &profile {
                c.store.import_names(&crate::tb::address_book(p));
                c.refresh_names();
                c.refresh_peers();
            }
            c.creds.set_tb_profile(profile);
            c.toast(if on { "Linked to Thunderbird" } else { "Thunderbird is no longer read" }, false);
        })
    });
    app.on_set_tab(|i| with_core(|c| c.set_tab(i)));
    app.on_open_chat(|key| with_core(|c| c.open_peer(key.to_string())));
    app.on_toggle_flag(|i, which| {
        with_core(|c| {
            let key = if i < 0 { c.selected.clone() } else { c.row_key(i) };
            if let Some(key) = key {
                c.toggle_flag(key, which);
            }
        })
    });
    app.on_mark_read_at(|i| {
        with_core(|c| {
            if let Some(key) = c.row_key(i) {
                let seen = c.store.mark_peer_seen(&key);
                c.flag(seen, true);
                c.refresh_peers();
            }
        })
    });
    app.on_test_notification(|| {
        with_core(|c| {
            crate::notify::show(&c.paths.data, "Thundergoose", "Notifications work. New mail will show up like this.", None);
            if c.settings.notify_sound {
                crate::notify::sound(&c.paths.data);
            }
        })
    });
    app.on_notify_changed(|| {
        with_core(|c| {
            let ui = c.ui();
            let app = ui.global::<App>();
            c.settings.notify_desktop = app.get_notify_desktop();
            c.settings.notify_sound = app.get_notify_sound();
            let _ = save_json(&c.paths.settings(), &c.settings, false);
            if c.settings.notify_sound {
                crate::notify::sound(&c.paths.data);
            }
        })
    });
    app.on_add_detect(|| {
        with_core(|c| {
            let email = c.ui().global::<App>().get_add_email().trim().to_string();
            if !email.contains('@') {
                c.add_status("Enter a valid email address", true, false);
                return;
            }
            c.add_status("Looking up the settings for this address…", false, true);
            std::thread::spawn(move || {
                let d = crate::autoconfig::detect(&email);
                on_ui(move |c| {
                    let ui = c.ui();
                    let app = ui.global::<App>();
                    let sec = |s: Security| match s {
                        Security::Tls => 0,
                        Security::StartTls => 1,
                        Security::Plain => 2,
                    };
                    app.set_add_imap_host(d.imap.host.as_str().into());
                    app.set_add_imap_port(d.imap.port.to_string().into());
                    app.set_add_imap_sec(sec(d.imap.security));
                    app.set_add_smtp_host(d.smtp.host.as_str().into());
                    app.set_add_smtp_port(d.smtp.port.to_string().into());
                    app.set_add_smtp_sec(sec(d.smtp.security));
                    app.set_add_username(d.imap.username.as_str().into());
                    app.set_add_oauth(match d.oauth {
                        Some(Issuer::Google) => 1,
                        Some(Issuer::Microsoft) => 2,
                        None => 0,
                    });
                    app.set_add_step(1);
                    let next = if d.oauth.is_some() { "Sign in continues in your browser." } else { "Enter your password." };
                    c.add_status(format!("Settings from {}. {next}", d.source), false, false);
                });
            });
        })
    });
    app.on_add_submit(|| {
        with_core(|c| {
            let (acct, password) = match c.form_account() {
                Ok(v) => v,
                Err(e) => {
                    c.add_status(e, true, false);
                    return;
                }
            };
            if c.accounts.iter().any(|a| a.id == acct.id) {
                c.add_status("This account is already set up", true, false);
                return;
            }
            let oauth = acct.auth != Auth::Password;
            c.add_status(if oauth { "Continue signing in in your browser…" } else { "Checking the servers…" }, false, true);
            let creds = c.creds.clone();
            std::thread::spawn(move || {
                let result = (|| {
                    if oauth {
                        creds.sign_in(&acct)?;
                    } else {
                        creds.set("imap", &acct.id, password.clone());
                        creds.set("smtp", &acct.id, password);
                    }
                    let mut s = crate::imapc::connect(&acct, &creds).map_err(|e| format!("Incoming server: {e}"))?;
                    let _ = s.logout();
                    crate::send::check(&acct, &creds).map_err(|e| format!("Outgoing server: {e}"))
                })();
                on_ui(move |c| match result {
                    Ok(()) => {
                        let email = acct.email.clone();
                        c.add_account(acct);
                        let ui = c.ui();
                        let app = ui.global::<App>();
                        app.set_add_open(false);
                        app.set_add_step(0);
                        for set in [App::set_add_email, App::set_add_password, App::set_add_name] {
                            set(&app, SharedString::new());
                        }
                        c.add_status("", false, false);
                        c.toast(format!("Added {email}. Fetching mail…"), false);
                    }
                    Err(e) => {
                        c.creds.forget(&acct.id);
                        c.add_status(e, true, false);
                    }
                });
            });
        })
    });
    app.on_remove_account(|id| with_core(|c| c.remove_account(&id)));
    app.on_appearance_changed(|| {
        with_core(|c| {
            let ui = c.ui();
        let app = ui.global::<App>();
            c.settings.accent = app.get_accent();
            c.settings.theme = app.get_theme_mode();
            let favicons_changed = app.get_show_favicons() != c.settings.favicons;
            c.settings.favicons = app.get_show_favicons();
            c.apply_appearance();
            let _ = save_json(&c.paths.settings(), &c.settings, false);
            if favicons_changed {
                c.refresh_peers();
                if c.selected.is_some() {
                    c.load_conversation(false);
                }
            }
        })
    });
    app.on_dismiss_toast(|| {
        with_core(|c| {
            c.toast_until = None;
            c.ui().global::<App>().set_toast(SharedString::new());
        })
    });

    let timer = slint::Timer::default();
    timer.start(slint::TimerMode::Repeated, Duration::from_millis(300), || with_core(|c| c.tick()));
    // Status changes don't always mark the store dirty; poll them separately.
    let status_timer = slint::Timer::default();
    status_timer.start(slint::TimerMode::Repeated, Duration::from_millis(1000), || with_core(|c| c.refresh_status()));
    Ok((ui, vec![timer, status_timer]))
}

/// Whether background work that changes the picture (icons, previews) is still going on.
pub(crate) fn busy() -> bool {
    let mut busy = false;
    with_core(|c| busy = c.favicons.busy() || c.previews_inflight > 0 || c.html_inflight > 0);
    busy
}
