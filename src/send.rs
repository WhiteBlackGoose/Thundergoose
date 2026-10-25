//! Composing and sending mail over SMTP.

use std::path::PathBuf;
use std::time::Duration;

use lettre::message::header::ContentType;
use lettre::message::{Attachment, Mailbox, MultiPart, SinglePart};
use lettre::transport::smtp::authentication::{Credentials, Mechanism};
use lettre::{Message, SmtpTransport, Transport};

use crate::config::{Account, Security};
use crate::creds::{Creds, Login};
use crate::store::{Header, Person};

pub struct Draft {
    pub account: Account,
    pub from: Person,
    pub to: Vec<Person>,
    pub subject: String,
    pub text: String,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    pub attachments: Vec<PathBuf>,
}

fn mailbox(p: &Person) -> Result<Mailbox, String> {
    let addr = p.addr.parse().map_err(|e| format!("invalid address {}: {e}", p.addr))?;
    Ok(Mailbox::new((!p.name.is_empty()).then(|| p.name.clone()), addr))
}

fn mime_for(path: &std::path::Path) -> ContentType {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let m = match ext.as_str() {
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "txt" | "md" | "log" => "text/plain",
        "html" | "htm" => "text/html",
        "csv" => "text/csv",
        "zip" => "application/zip",
        "gz" => "application/gzip",
        "tar" => "application/x-tar",
        "json" => "application/json",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "odt" => "application/vnd.oasis.opendocument.text",
        "ics" => "text/calendar",
        "mp3" => "audio/mpeg",
        "mp4" => "video/mp4",
        _ => "application/octet-stream",
    };
    ContentType::parse(m).unwrap_or(ContentType::TEXT_PLAIN)
}

fn transport(account: &Account, creds: &Creds) -> Result<SmtpTransport, String> {
    let s = &account.smtp;
    let builder = match s.security {
        Security::Tls => SmtpTransport::relay(&s.host),
        Security::StartTls => SmtpTransport::starttls_relay(&s.host),
        Security::Plain => Ok(SmtpTransport::builder_dangerous(&s.host)),
    }
    .map_err(|e| format!("{}: {e}", s.host))?;
    let (credentials, mechanisms) = match creds.smtp(account).map_err(|e| e.to_string())? {
        Login::Password { user, password } => (Credentials::new(user, password), vec![Mechanism::Plain, Mechanism::Login]),
        Login::XOAuth2 { user, token } => (Credentials::new(user, token), vec![Mechanism::Xoauth2]),
    };
    Ok(builder.port(s.port).credentials(credentials).authentication(mechanisms).timeout(Some(Duration::from_secs(90))).build())
}

/// Connects and logs in to the outgoing server without sending anything.
pub fn check(account: &Account, creds: &Creds) -> Result<(), String> {
    match transport(account, creds)?.test_connection() {
        Ok(true) => Ok(()),
        Ok(false) => Err("connection closed".into()),
        Err(e) => Err(e.to_string()),
    }
}

fn new_message_id(from: &str) -> String {
    let mut buf = [0u8; 12];
    getrandom::fill(&mut buf).ok();
    let domain = from.rsplit('@').next().unwrap_or("thundergoose.local");
    format!("{}.{}@{domain}", chrono::Utc::now().timestamp_millis(), buf.iter().map(|b| format!("{b:02x}")).collect::<String>())
}

/// Sends the draft. Returns the raw message and its header for the local store.
pub fn send(d: &Draft, creds: &Creds) -> Result<(Vec<u8>, Header), String> {
    let msgid = new_message_id(&d.from.addr);
    let mut b = Message::builder().from(mailbox(&d.from)?).subject(d.subject.clone()).message_id(Some(format!("<{msgid}>"))).user_agent("Thundergoose".into());
    for p in &d.to {
        b = b.to(mailbox(p)?);
    }
    if let Some(irt) = &d.in_reply_to {
        b = b.in_reply_to(format!("<{irt}>"));
        let refs: Vec<String> = d.references.iter().map(|r| format!("<{r}>")).collect();
        if !refs.is_empty() {
            b = b.references(refs.join(" "));
        }
    }
    let msg = if d.attachments.is_empty() {
        b.header(ContentType::TEXT_PLAIN).body(d.text.clone())
    } else {
        let mut mp = MultiPart::mixed().singlepart(SinglePart::plain(d.text.clone()));
        for path in &d.attachments {
            let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("attachment").to_string();
            mp = mp.singlepart(Attachment::new(name).body(bytes, mime_for(path)));
        }
        b.multipart(mp)
    }
    .map_err(|e| format!("cannot build message: {e}"))?;

    let transport = transport(&d.account, creds)?;
    transport.send(&msg).map_err(|e| {
        if e.is_permanent() {
            creds.invalidate(&d.account);
        }
        format!("{}: {e}", d.account.smtp.host)
    })?;

    let raw = msg.formatted();
    let header = Header {
        msgid,
        date: chrono::Utc::now().timestamp(),
        from: d.from.clone(),
        to: d.to.clone(),
        cc: Vec::new(),
        subject: d.subject.clone(),
        in_reply_to: d.in_reply_to.clone().unwrap_or_default(),
        size: raw.len() as u32,
        seen: true,
    };
    Ok((raw, header))
}
