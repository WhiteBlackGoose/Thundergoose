//! Turning IMAP envelopes and raw RFC 5322 messages into what the chat view shows.

use mail_parser::{MessageParser, MimeHeaders};

use crate::store::{Attachment, Body, Header, Person};

pub fn norm_id(s: &str) -> String {
    s.trim().trim_start_matches('<').trim_end_matches('>').trim().to_string()
}

fn lossy(b: Option<&[u8]>) -> String {
    b.map(|b| String::from_utf8_lossy(b).into_owned()).unwrap_or_default()
}

/// Decodes RFC 2047 encoded words by letting mail-parser read the text as a Subject header.
pub fn decode_words(raw: &str) -> String {
    if !raw.contains("=?") {
        return raw.trim().to_string();
    }
    let hdr = format!("Subject: {}\r\n\r\n", raw.replace(['\r', '\n'], " "));
    MessageParser::default().parse(hdr.as_bytes()).and_then(|m| m.subject().map(str::to_string)).unwrap_or_else(|| raw.to_string()).trim().to_string()
}

fn people(list: Option<&Vec<imap_proto::types::Address<'_>>>) -> Vec<Person> {
    list.into_iter()
        .flatten()
        .filter_map(|a| {
            let mailbox = lossy(a.mailbox.as_deref());
            let host = lossy(a.host.as_deref());
            // Group syntax shows up as entries without a host; skip those markers.
            if mailbox.is_empty() || host.is_empty() {
                return None;
            }
            let addr = format!("{mailbox}@{host}");
            Some(Person { name: decode_words(&lossy(a.name.as_deref())).trim_matches(['"', '\'']).trim().to_string(), addr })
        })
        .collect()
}

pub fn parse_date(raw: &str) -> Option<i64> {
    let hdr = format!("Date: {raw}\r\n\r\n");
    let m = MessageParser::default().parse(hdr.as_bytes())?;
    m.date().map(|d| d.to_timestamp()).filter(|&t| t > 0)
}

pub fn header_from_fetch(f: &imap::types::Fetch<'_>) -> Option<Header> {
    let env = f.envelope()?;
    let from = people(env.from.as_ref()).into_iter().next().or_else(|| people(env.sender.as_ref()).into_iter().next()).unwrap_or(Person { name: String::new(), addr: String::new() });
    let to = people(env.to.as_ref());
    let cc = people(env.cc.as_ref());
    let subject = decode_words(&lossy(env.subject.as_deref()));
    let internal = f.internal_date().map(|d| d.timestamp());
    let date = env.date.as_deref().and_then(|d| parse_date(&String::from_utf8_lossy(d))).or(internal).unwrap_or(0);
    let msgid = env.message_id.as_deref().map(|m| norm_id(&String::from_utf8_lossy(m))).filter(|m| !m.is_empty()).unwrap_or_else(|| {
        // No Message-ID: derive a stable one so duplicates across folders still merge.
        use sha2::Digest;
        let h = sha2::Sha256::digest(format!("{date}|{}|{subject}", from.addr));
        format!("tg-{}@local", h.iter().take(12).map(|b| format!("{b:02x}")).collect::<String>())
    });
    let seen = f.flags().iter().any(|fl| matches!(fl, imap::types::Flag::Seen));
    Some(Header {
        msgid,
        date,
        from,
        to,
        cc,
        subject,
        in_reply_to: env.in_reply_to.as_deref().map(|r| norm_id(&String::from_utf8_lossy(r))).unwrap_or_default(),
        size: f.size.unwrap_or(0),
        seen,
    })
}

fn is_attribution(line: &str, next: Option<&str>) -> bool {
    let l = line.trim();
    if l.len() > 300 {
        return false;
    }
    let lower = l.to_lowercase();
    let ends = |s: &str| lower.ends_with(s);
    if ends("wrote:") || ends("schrieb:") || ends("a écrit :") || ends("a écrit:") || ends("escribió:") || ends("написал:") || ends("написал(а):") || ends("scrisse:") || ends("schreef:") || ends("napisał:") {
        return lower.starts_with("on ") || lower.starts_with("am ") || lower.starts_with("le ") || lower.starts_with("el ") || lower.contains('@') || lower.contains(',') || lower.chars().any(|c| c.is_ascii_digit());
    }
    // "Am 12. Oktober 2026 um 08:08 schrieb Name <a@b.c>:" (the verb comes before the name).
    if lower.ends_with(':') && [" schrieb ", " wrote ", " a écrit ", " escribió ", " написал ", " scrisse ", " schreef ", " napisał "].iter().any(|v| lower.contains(v)) && (lower.contains('@') || lower.chars().any(|c| c.is_ascii_digit())) {
        return true;
    }
    // "On Mon, 1 Jan 2024 at 10:00, Someone <a@b.c>" + "wrote:" on the next line.
    if (lower.starts_with("on ") || lower.starts_with("am ")) && next.map(|n| n.trim().to_lowercase()).is_some_and(|n| n == "wrote:" || n == "schrieb:") {
        return true;
    }
    let dashes = l.trim_matches('-').trim().to_lowercase();
    if l.starts_with("-----") && matches!(dashes.as_str(), "original message" | "ursprüngliche nachricht" | "original nachricht" | "message d'origine" | "mensaje original" | "исходное сообщение") {
        return true;
    }
    false
}

fn outlook_header_block(lines: &[&str], i: usize) -> bool {
    let l = lines[i].trim();
    // Outlook writes "Von: x" or, in some versions, "Von:x".
    let starts = |s: &str, ps: &[&str]| {
        let s = s.to_lowercase();
        ps.iter().any(|p| s.starts_with(p))
    };
    if !starts(l, &["from:", "von:", "de :", "de:", "от:"]) && !(l.chars().all(|c| c == '_') && l.len() >= 20) {
        return false;
    }
    lines[i + 1..].iter().take(5).any(|n| starts(n.trim(), &["sent:", "gesendet:", "date:", "datum:", "envoyé :", "envoyé:", "enviado:", "отправлено:"]))
}

/// Drops invisible characters that newsletters use to pad their preview text.
pub fn clean(text: &str) -> String {
    text.chars()
        .filter(|c| !matches!(c, '\u{34f}' | '\u{ad}' | '\u{200b}'..='\u{200f}' | '\u{2060}'..='\u{2064}' | '\u{feff}' | '\u{180e}'))
        .map(|c| if matches!(c, '\u{2028}' | '\u{2029}') { '\n' } else { c })
        .collect()
}

/// HTML converted to text puts a blank line after every paragraph, and Outlook makes every
/// line a paragraph. One blank line becomes a line break; two or more become one blank line.
fn tighten_html_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blanks = 0;
    for line in text.lines() {
        if line.trim().is_empty() {
            blanks += 1;
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
            if blanks >= 2 {
                out.push('\n');
            }
        }
        blanks = 0;
        out.push_str(line.trim_end());
    }
    out
}

/// Cuts the quoted previous conversation off a reply. Returns (text, whether anything was cut).
pub fn strip_quotes(text: &str) -> (String, bool) {
    let text = clean(text).replace("\r\n", "\n").replace('\u{a0}', " ");
    let lines: Vec<&str> = text.lines().collect();
    let mut cut = None;
    for i in 0..lines.len() {
        let l = lines[i];
        if is_attribution(l, lines.get(i + 1).copied()) || outlook_header_block(&lines, i) {
            cut = Some(i);
            break;
        }
        if l.trim_start().starts_with('>') && lines[i..].iter().all(|r| r.trim().is_empty() || r.trim_start().starts_with('>')) {
            cut = Some(i);
            break;
        }
    }
    let kept: &[&str] = match cut {
        Some(i) if lines[..i].iter().any(|l| !l.trim().is_empty()) => &lines[..i],
        _ => &lines[..],
    };
    let mut out = String::new();
    let mut blank = 0;
    for l in kept {
        let l = l.trim_end();
        if l.is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        out.push_str(l);
        out.push('\n');
    }
    let stripped = cut.is_some() && kept.len() < lines.len();
    (out.trim().to_string(), stripped)
}

pub fn snippet(text: &str) -> String {
    let flat: String = clean(text).split_whitespace().collect::<Vec<_>>().join(" ");
    let mut s: String = flat.chars().take(160).collect();
    if flat.chars().count() > 160 {
        s.push('…');
    }
    s
}

/// The readable text of a message (HTML converted when there is no plain part).
pub fn full_text(raw: &[u8]) -> String {
    let Some(m) = MessageParser::default().parse(raw) else { return String::new() };
    let text = clean(&m.body_text(0).map(|t| t.into_owned()).unwrap_or_default());
    if m.text_part(0).is_some_and(|p| p.is_text_html()) {
        tighten_html_text(&text)
    } else {
        text
    }
}

pub fn html(raw: &[u8]) -> Option<String> {
    let m = MessageParser::default().parse(raw)?;
    if !m.html_part(0).is_some_and(|p| p.is_text_html()) {
        return None;
    }
    m.body_html(0).map(|h| h.into_owned())
}

/// Parts an HTML body refers to by Content-ID (`cid:`), keyed without angle brackets.
pub fn inline_parts(raw: &[u8]) -> std::collections::HashMap<String, Vec<u8>> {
    let Some(m) = MessageParser::default().parse(raw) else { return Default::default() };
    m.parts.iter().filter_map(|p| Some((p.content_id()?.trim_matches(['<', '>']).to_string(), p.contents().to_vec()))).collect()
}

pub fn attachment_bytes(raw: &[u8], index: usize) -> Option<Vec<u8>> {
    let m = MessageParser::default().parse(raw)?;
    let part = m.attachment(index as u32)?;
    Some(part.contents().to_vec())
}

/// Inline images at least this big are photos rather than logos, and listed as attachments.
const INLINE_PHOTO_MIN: usize = 100 * 1024;

pub fn body(raw: &[u8]) -> Body {
    let Some(m) = MessageParser::default().parse(raw) else {
        return Body { rich: false, text: String::new(), quoted: false, has_html: false, attachments: Vec::new(), refs: String::new(), snippet: String::new() };
    };
    let mut full = clean(&m.body_text(0).map(|t| t.into_owned()).unwrap_or_default());
    if m.text_part(0).is_some_and(|p| p.is_text_html()) {
        full = tighten_html_text(&full);
    }
    let (text, quoted) = strip_quotes(&full);
    // mail-parser lists plain text parts as HTML bodies too; only a real HTML part counts.
    let has_html = m.html_part(0).is_some_and(|p| p.is_text_html());
    let attachments = m
        .attachments()
        .enumerate()
        .filter_map(|(index, part)| {
            let name = part.attachment_name().map(str::to_string);
            let attached = part.content_disposition().is_some_and(|d| d.is_attachment());
            // Unnamed inline parts and images referenced by Content-ID are part of an HTML body
            // (signature logos and the like), not files someone sent.
            // Phones send photos inline too, though; those are big.
            let mime = part.content_type().map(|c| format!("{}/{}", c.ctype(), c.subtype().unwrap_or("octet-stream"))).unwrap_or_else(|| "application/octet-stream".into()).to_lowercase();
            let photo = mime.starts_with("image/") && part.contents().len() >= INLINE_PHOTO_MIN;
            if !attached && !photo && (name.is_none() || part.content_id().is_some()) {
                return None;
            }
            let name = name.unwrap_or_else(|| {
                if part.is_message() {
                    "message.eml".into()
                } else if let Some(ext) = mime.strip_prefix("image/") {
                    format!("image.{}", if ext == "jpeg" { "jpg" } else { ext })
                } else {
                    "attachment".into()
                }
            });
            Some(Attachment { name, mime, size: part.contents().len(), index })
        })
        .collect();
    let refs = m.references().as_text_list().map(|l| l.iter().map(|r| norm_id(r)).collect::<Vec<_>>().join(" ")).or_else(|| m.references().as_text().map(norm_id)).unwrap_or_default();
    // Short, chat-like mail previews its text; long ones (newsletters, receipts) their subject.
    let shown = if text.is_empty() { &full } else { &text };
    let snip = match m.subject() {
        Some(subj) if shown.chars().count() > 280 && !subj.trim().is_empty() => snippet(subj),
        _ => snippet(shown),
    };
    let rich = has_html && m.body_html(0).is_some_and(|h| crate::html::is_rich(&h));
    Body { rich, text: if text.is_empty() { full.trim().to_string() } else { text }, quoted, has_html, attachments, refs, snippet: snip }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_gmail_quote() {
        let t = "Sounds good!\n\nOn Mon, 3 Jun 2024 at 10:12, Alice <a@b.c> wrote:\n> Lunch?\n> \n";
        assert_eq!(strip_quotes(t), ("Sounds good!".to_string(), true));
    }

    #[test]
    fn strips_two_line_attribution() {
        let t = "Yes\nOn Mon, 3 Jun 2024 at 10:12, Alice Wonderland <a@b.c>\nwrote:\n> Lunch?";
        assert_eq!(strip_quotes(t).0, "Yes");
    }

    #[test]
    fn strips_german_attribution() {
        let t = "Gibt es ein Update?\n\nAm 3. Juni 2024 08:08:24 MESZ schrieb Alice <a@example.org>:\n>Hallo";
        assert_eq!(strip_quotes(t).0, "Gibt es ein Update?");
    }

    #[test]
    fn strips_outlook_block() {
        let t = "Danke!\n\n________________________________\nVon: Bob <b@c.d>\nGesendet: Montag\nAn: me\nBetreff: x\n\nold";
        assert_eq!(strip_quotes(t).0, "Danke!");
    }

    #[test]
    fn strips_outlook_without_spaces() {
        let t = "Thanks!\n Von:Bob <bob@example.org>\n Gesendet:Montag, 3. Juni 2024 14:22\n An:Alice\nold";
        assert_eq!(strip_quotes(t).0, "Thanks!");
    }

    #[test]
    fn keeps_fully_quoted() {
        let t = "> only a quote\n> here";
        assert_eq!(strip_quotes(t).0, t);
    }

    #[test]
    fn keeps_inline_replies() {
        let t = "> question 1\nanswer 1\n> question 2\nanswer 2";
        assert_eq!(strip_quotes(t).0, t);
    }

    #[test]
    fn decodes_words() {
        assert_eq!(decode_words("=?UTF-8?B?0J/RgNC40LLQtdGC?="), "Привет");
    }
}
