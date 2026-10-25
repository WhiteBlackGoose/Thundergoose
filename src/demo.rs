//! Made-up mail for trying Thundergoose out and for the screenshots in the README
//! (`thundergoose --demo`). Everybody here is fictional and uses `.example` addresses; the
//! service senders exist only so their website icons show.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, SystemTime};

use chrono::{Local, TimeZone};
use lettre::message::header::ContentType;
use lettre::message::{Attachment, Mailbox, MultiPart, SinglePart};
use lettre::Message;

use crate::config::{save_json, Account, Auth, Paths, Security, Server, Settings};
use crate::store::{self, Header, Person, Store};

const PEOPLE: &[(&str, &str)] = &[
    ("Sam Rivera", "sam@rivera.example"),
    ("Sam Rivera", "sam.r@work.example"),
    ("Maya Chen", "maya@chen.example"),
    ("Leo Park", "leo@park.example"),
    ("Priya Natarajan", "priya@natarajan.example"),
    ("Mom", "mom@rivera.example"),
    ("Jonas Becker", "jonas@becker.example"),
    ("Aiko Tanaka", "aiko@tanaka.example"),
    ("GitHub", "notifications@github.com"),
    ("Codeberg", "noreply@codeberg.org"),
    ("NixOS Discourse", "discourse@discourse.nixos.org"),
    ("Rust Users Forum", "digest@users.rust-lang.org"),
    ("Mega Deals", "deals@mega-deals.example"),
];
const ME: usize = 0;
const WORK: usize = 1;
const MAYA: usize = 2;
const LEO: usize = 3;
const PRIYA: usize = 4;
const MOM: usize = 5;
const JONAS: usize = 6;
const AIKO: usize = 7;
const GITHUB: usize = 8;
const CODEBERG: usize = 9;
const NIXOS: usize = 10;
const RUST: usize = 11;
const DEALS: usize = 12;

enum File {
    Photo(u8, &'static str),
    Pdf(&'static str),
}

struct Mail {
    /// Days before today, and the time of day.
    when: (i64, u32, u32),
    from: usize,
    to: &'static [usize],
    subject: &'static str,
    text: &'static str,
    seen: bool,
    file: Option<File>,
}

const fn m(when: (i64, u32, u32), from: usize, to: &'static [usize], subject: &'static str, text: &'static str) -> Mail {
    Mail { when, from, to, subject, text, seen: true, file: None }
}

impl Mail {
    const fn unread(mut self) -> Self {
        self.seen = false;
        self
    }
    const fn with(mut self, f: File) -> Self {
        self.file = Some(f);
        self
    }
}

fn mails() -> Vec<Mail> {
    vec![
        // Maya
        m((3, 19, 2), MAYA, &[ME], "October", "Did you end up booking the cabin for October?"),
        m((3, 19, 30), ME, &[MAYA], "Re: October", "Yes! Lake Toya, three nights. I'll send you the details later."),
        m((1, 18, 40), MAYA, &[ME], "Re: October", "Can't wait. Should I bring the camera?"),
        m((1, 18, 52), ME, &[MAYA], "Re: October", "Please do. And the tripod, the stars up there are unreal."),
        m((0, 9, 12), MAYA, &[ME], "From last year", "Look what I found from last year's trip").with(File::Photo(0, "lake-toya.jpg")),
        m((0, 9, 20), ME, &[MAYA], "Re: From last year", "Wow, that sunset! You should frame it."),
        m((0, 9, 31), MAYA, &[ME], "Re: From last year", "Haha, maybe. See you on Saturday!"),
        // Maya, Leo, Priya
        m((0, 8, 2), MAYA, &[ME, LEO, PRIYA], "Saturday hike", "Hi all! Saturday, 8:00 at the trailhead parking? The weather looks great."),
        m((0, 8, 15), LEO, &[ME, MAYA, PRIYA], "Re: Saturday hike", "I'm in. I can drive two people."),
        m((0, 8, 40), PRIYA, &[ME, MAYA, LEO], "Re: Saturday hike", "Count me in, I'll bring snacks."),
        m((0, 8, 52), ME, &[MAYA, LEO, PRIYA], "Re: Saturday hike", "Perfect, see you all at 8! I'll bring the map and a thermos of tea."),
        m((0, 10, 3), LEO, &[ME, MAYA, PRIYA], "Re: Saturday hike", "Do we need to reserve parking?").unread(),
        // Leo
        m((1, 12, 5), LEO, &[ME], "Bike repair", "Here's the invoice for the bike repair. Thanks again for lending me the tools!").with(File::Pdf("invoice-0815.pdf")),
        m((1, 13, 0), ME, &[LEO], "Re: Bike repair", "Thanks, paid!"),
        m((0, 11, 5), LEO, &[ME], "Tent", "By the way, do you still have my tent?").unread(),
        m((0, 11, 6), LEO, &[ME], "Re: Tent", "No rush, just need it by Friday.").unread(),
        // Priya, at work
        m((2, 9, 14), PRIYA, &[WORK], "Q4 roadmap draft", "I've put the roadmap draft in the shared folder. Could you review the onboarding section before Thursday?"),
        m((2, 9, 40), WORK, &[PRIYA], "Re: Q4 roadmap draft", "Sure, you'll have my comments by Wednesday evening."),
        m((2, 9, 41), PRIYA, &[WORK], "Re: Q4 roadmap draft", "Perfect, thank you!"),
        m((0, 8, 30), WORK, &[PRIYA], "Re: Q4 roadmap draft", "Done. I left comments on onboarding and on the import flow."),
        // Mom
        m((1, 20, 15), MOM, &[ME], "Sunday", "Are you coming for dinner on Sunday? Dad is making paella."),
        m((1, 20, 31), ME, &[MOM], "Re: Sunday", "Of course! I'll bring dessert."),
        // Jonas, Aiko
        m((4, 16, 0), JONAS, &[WORK], "Slides", "Great talk yesterday! Could you send me the slides?"),
        m((4, 16, 45), WORK, &[JONAS], "Re: Slides", "Here you go. The demo part is in the appendix.").with(File::Pdf("mail-as-conversations.pdf")),
        m((6, 21, 10), AIKO, &[ME], "The book", "Thank you for the book recommendation! I finished it in two evenings."),
        // Services
        m((2, 15, 3), GITHUB, &[ME], "[rivera/thundergoose] Release v0.2.0", "v0.2.0 has been published.\n\nWebsite icons, image previews and a Yourself conversation."),
        m((0, 7, 58), GITHUB, &[ME], "Re: [rivera/thundergoose] Quick actions in the chat list (#42)", "maya-chen approved these changes.\n\nLooks great, merging after CI.").unread(),
        m((1, 6, 30), CODEBERG, &[ME], "New sign-in to your account", "A new sign-in to your account was detected from Firefox on Linux.").unread(),
        m((0, 6, 0), NIXOS, &[ME], "[NixOS Discourse] Summary", "Popular topics this week: flakes in practice, a new installer, and the 26.11 release schedule.").unread(),
        m((5, 6, 0), RUST, &[ME], "[Rust Users] Weekly digest", "Popular topics: async traits in practice, a GUI with Slint, and error handling patterns."),
        m((0, 5, 12), DEALS, &[ME], "Only today: 70% off everything!!!", "Don't miss out! Our biggest sale of the year ends at midnight.").unread(),
        // Yourself
        m((5, 22, 4), ME, &[WORK], "Cabin", "Cabin Wi-Fi: lakeview-guest\nPassword: pinecone42\nCheck-in after 15:00, key box code 2741."),
        m((0, 7, 50), WORK, &[ME], "Whiteboard", "Whiteboard from the planning session").with(File::Photo(1, "whiteboard.jpg")),
    ]
}

fn mailbox(i: usize) -> Mailbox {
    let (name, addr) = PEOPLE[i];
    Mailbox::new(Some(name.into()), addr.parse().expect("demo address"))
}

fn person(i: usize) -> Person {
    let (name, addr) = PEOPLE[i];
    Person { name: name.into(), addr: addr.into() }
}

fn account(name: &str, email: &str) -> Account {
    let server = |host: &str, port| Server { host: host.into(), port, security: Security::Tls, username: email.into() };
    Account { id: email.into(), email: email.into(), name: name.into(), aliases: Vec::new(), imap: server("imap.example", 993), smtp: server("smtp.example", 465), auth: Auth::Password, save_sent: false }
}

/// A small, valid one-page PDF.
fn pdf(title: &str) -> Vec<u8> {
    let content = format!("BT /F1 24 Tf 72 720 Td ({title}) Tj ET");
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_string(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
    ];
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1).as_bytes());
    out
}

fn lerp(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    let t = t.clamp(0.0, 1.0);
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

fn rgb(hex: u32) -> [f32; 3] {
    [((hex >> 16) & 255) as f32, ((hex >> 8) & 255) as f32, (hex & 255) as f32]
}

/// A painted landscape: a sunset over a lake (0) or a misty morning (1).
fn photo(variant: u8) -> Vec<u8> {
    let (w, h) = (1400u32, 930u32);
    let (sky, ridges, sun_color) = match variant {
        0 => ([rgb(0x2b1d5c), rgb(0xc2477a), rgb(0xff9f5a), rgb(0xffe2a8)], [rgb(0x6b3f78), rgb(0x4a2a62), rgb(0x2a1840)], rgb(0xfff1c9)),
        _ => ([rgb(0x9fc6d9), rgb(0xcfe3e6), rgb(0xf2efe2), rgb(0xfdf8ec)], [rgb(0x86a8a6), rgb(0x55807a), rgb(0x2f5550)], rgb(0xffffff)),
    };
    let horizon = 0.64f32;
    let ridge = |x: f32, i: usize| {
        let s = i as f32 * 1.7 + variant as f32;
        let base = 0.40 + i as f32 * 0.075;
        base - 0.09 * (0.55 * (x * 3.1 + s).sin() + 0.3 * (x * 7.9 + s * 2.3).sin() + 0.15 * (x * 17.0 + s * 3.1).sin())
    };
    let (sun_x, sun_y, sun_r) = (0.68f32, 0.47f32, 0.06f32);
    let aspect = w as f32 / h as f32;
    let scene = |x: f32, y: f32| -> [f32; 3] {
        let t = (y / horizon).clamp(0.0, 1.0);
        let mut c = if t < 0.45 { lerp(sky[0], sky[1], t / 0.45) } else if t < 0.8 { lerp(sky[1], sky[2], (t - 0.45) / 0.35) } else { lerp(sky[2], sky[3], (t - 0.8) / 0.2) };
        let d = (((x - sun_x) * aspect).powi(2) + (y - sun_y).powi(2)).sqrt();
        c = lerp(c, sun_color, (-d / 0.12).exp() * 0.55);
        if d < sun_r {
            c = sun_color;
        }
        for (i, rc) in ridges.iter().enumerate() {
            let top = ridge(x, i);
            if y > top {
                let haze = 0.35 * (1.0 - i as f32 / 3.0);
                c = lerp(*rc, sky[2], haze * (1.0 - ((y - top) * 3.0).min(1.0)) + haze * 0.4);
            }
        }
        c
    };
    let mut img = image::RgbImage::new(w, h);
    for (px, py, p) in img.enumerate_pixels_mut() {
        let (x, y) = (px as f32 / w as f32, py as f32 / h as f32);
        let c = if y < horizon {
            scene(x, y)
        } else {
            let depth = (y - horizon) / (1.0 - horizon);
            let ripple = 0.004 * (y * 420.0).sin() * (1.0 + depth * 2.0);
            let mirrored = scene(x + ripple, 2.0 * horizon - y);
            let water = lerp(ridges[2], sky[0], 0.3);
            lerp(mirrored, water, 0.35 + depth * 0.45)
        };
        *p = image::Rgb([c[0] as u8, c[1] as u8, c[2] as u8]);
    }
    let mut out = Vec::new();
    let _ = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 88).encode_image(&img);
    out
}

fn raw_message(n: usize, mail: &Mail, date: i64) -> Vec<u8> {
    let mut b = Message::builder()
        .from(mailbox(mail.from))
        .subject(mail.subject)
        .date(SystemTime::UNIX_EPOCH + Duration::from_secs(date as u64))
        .message_id(Some(format!("<demo-{n}@thundergoose.example>")));
    for &t in mail.to {
        b = b.to(mailbox(t));
    }
    let msg = match &mail.file {
        None => b.header(ContentType::TEXT_PLAIN).body(mail.text.to_string()),
        Some(file) => {
            let (name, mime, bytes) = match file {
                File::Photo(v, name) => (*name, "image/jpeg", photo(*v)),
                File::Pdf(name) => (*name, "application/pdf", pdf(mail.subject)),
            };
            let att = Attachment::new(name.to_string()).body(bytes, ContentType::parse(mime).expect("mime"));
            b.multipart(MultiPart::mixed().singlepart(SinglePart::plain(mail.text.to_string())).singlepart(att))
        }
    };
    msg.expect("demo message").formatted()
}

/// Points the data directory at a fresh demo profile and fills it.
pub fn prepare() {
    let dir = std::env::temp_dir().join("thundergoose-demo");
    // Keep downloaded icons between runs, start over with everything else.
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            if e.file_name() != "favicons" {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    std::env::set_var("THUNDERGOOSE_HOME", &dir);
    let paths = Paths::new();
    let accounts = vec![account("Sam Rivera", PEOPLE[ME].1), account("Sam Rivera", PEOPLE[WORK].1)];
    let _ = save_json(&paths.accounts(), &accounts, false);
    let _ = save_json(&paths.settings(), &Settings::default(), false);
    if let Err(e) = seed(&paths, &accounts) {
        eprintln!("demo: {e}");
    }
}

fn seed(paths: &Paths, accounts: &[Account]) -> rusqlite::Result<()> {
    let store = Store::open(&paths.db())?;
    store.conn.execute_batch(&format!("PRAGMA user_version = {}", store::PARSE_VERSION))?;
    let me: HashSet<String> = accounts.iter().map(|a| a.email.clone()).collect();
    let names: HashMap<String, String> = PEOPLE.iter().skip(2).map(|(n, a)| (a.to_string(), n.to_string())).collect();
    store.import_names(&names);
    let today = Local::now().date_naive();
    for (n, mail) in mails().iter().enumerate() {
        let (days, hh, mm) = mail.when;
        let day = today - chrono::Duration::days(days);
        let date = Local.from_local_datetime(&day.and_hms_opt(hh, mm, 0).expect("time")).single().map(|t| t.timestamp()).unwrap_or(0);
        let raw = raw_message(n, mail, date);
        let account = if mail.from == WORK || mail.to.contains(&WORK) { &accounts[1] } else { &accounts[0] };
        let header = Header {
            msgid: format!("demo-{n}@thundergoose.example"),
            date,
            from: person(mail.from),
            to: mail.to.iter().map(|&t| person(t)).collect(),
            cc: Vec::new(),
            subject: mail.subject.into(),
            in_reply_to: String::new(),
            size: raw.len() as u32,
            seen: mail.seen || mail.from <= WORK,
        };
        let placement = store::place(&header, &me, &account.email);
        let (id, _) = store.add(&account.id, "INBOX", n as u32 + 1, &header, &placement)?;
        store.set_body(id, &raw, &crate::parse::body(&raw))?;
    }
    store.set_flag(PEOPLE[MAYA].1, store::Flag::Favorite, true);
    store.set_flag(PEOPLE[PRIYA].1, store::Flag::Favorite, true);
    store.set_flag(PEOPLE[MOM].1, store::Flag::Favorite, true);
    store.set_flag(PEOPLE[CODEBERG].1, store::Flag::Muted, true);
    store.set_flag(PEOPLE[DEALS].1, store::Flag::Ignored, true);
    Ok(())
}
