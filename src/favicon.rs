//! Website icons for contacts at companies and organisations. Addresses at mailbox providers
//! (Gmail, Outlook, GMX, …) keep their initials: the provider's logo says nothing about the person.
//!
//! Icons are fetched in the background from the domain's own website, scaled to 64 px and cached
//! in `favicons/`. A domain without a usable icon is remembered and asked again after a month.

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

const WORKERS: usize = 4;
const SIZE: u32 = 64;
const RETRY_AFTER: Duration = Duration::from_secs(30 * 86400);
const MAX_DOWNLOAD: u64 = 2 * 1024 * 1024;

/// Mailbox providers: domains where anybody can have an address.
const PROVIDERS: &[&str] = &[
    "gmail.com", "googlemail.com", "outlook.com", "hotmail.com", "live.com", "msn.com", "passport.com",
    "yahoo.com", "ymail.com", "rocketmail.com", "aol.com", "aim.com", "icloud.com", "me.com", "mac.com",
    "mail.com", "email.com", "usa.com", "gmx.com", "gmx.net", "gmx.de", "gmx.at", "gmx.ch", "web.de",
    "t-online.de", "freenet.de", "arcor.de", "online.de", "1und1.de", "posteo.de", "posteo.net",
    "mailbox.org", "proton.me", "protonmail.com", "protonmail.ch", "pm.me", "tutanota.com", "tutanota.de",
    "tutamail.com", "tuta.io", "tuta.com", "fastmail.com", "fastmail.fm", "hey.com", "zoho.com", "zohomail.com",
    "yandex.ru", "yandex.com", "ya.ru", "mail.ru", "inbox.ru", "list.ru", "bk.ru", "rambler.ru", "ukr.net",
    "qq.com", "foxmail.com", "163.com", "126.com", "yeah.net", "sina.com", "sohu.com", "aliyun.com",
    "naver.com", "daum.net", "hanmail.net", "seznam.cz", "centrum.cz", "wp.pl", "o2.pl", "onet.pl",
    "interia.pl", "libero.it", "virgilio.it", "tiscali.it", "alice.it", "orange.fr", "wanadoo.fr",
    "free.fr", "laposte.net", "sfr.fr", "bluewin.ch", "telenet.be", "skynet.be", "ziggo.nl", "kpnmail.nl",
    "comcast.net", "verizon.net", "att.net", "sbcglobal.net", "bellsouth.net", "cox.net", "charter.net",
    "earthlink.net", "btinternet.com", "sky.com", "virginmedia.com", "talktalk.net", "rediffmail.com",
    "hushmail.com", "disroot.org", "riseup.net", "cock.li", "gmx.us", "inbox.lv", "abv.bg", "rogers.com",
    "shaw.ca", "bigpond.com", "optusnet.com.au", "xtra.co.nz", "uol.com.br", "bol.com.br", "terra.com.br",
];

/// Provider families with many country domains (yahoo.co.jp, hotmail.fr, live.de, …).
const PROVIDER_FAMILIES: &[&str] = &["yahoo", "hotmail", "outlook", "live", "gmx", "yandex", "aol", "windowslive"];

pub fn is_provider(domain: &str) -> bool {
    let d = domain.trim_end_matches('.').to_ascii_lowercase();
    PROVIDERS.iter().any(|p| d == *p || d.ends_with(&format!(".{p}")))
        || PROVIDER_FAMILIES.iter().any(|f| d.starts_with(&format!("{f}.")) && d.split('.').count() <= 3)
}

/// The domain whose website stands for `addr`, if it's an organisation and not a mailbox provider.
pub fn domain_for(addr: &str) -> Option<String> {
    let domain = addr.rsplit_once('@')?.1.trim().to_ascii_lowercase();
    if !domain.contains('.') || is_provider(&domain) {
        return None;
    }
    let base = crate::autoconfig::base_domain(&domain);
    (!is_provider(&base)).then_some(base)
}

pub struct Favicons {
    dir: PathBuf,
    queue: Mutex<VecDeque<String>>,
    wake: Condvar,
    pending: Mutex<HashSet<String>>,
    /// Set when an icon was stored, so the UI can pick it up.
    pub updated: AtomicBool,
}

impl Favicons {
    pub fn new(data: &Path) -> Arc<Self> {
        let dir = data.join("favicons");
        let _ = std::fs::create_dir_all(&dir);
        let me = Arc::new(Favicons { dir, queue: Mutex::new(VecDeque::new()), wake: Condvar::new(), pending: Mutex::new(HashSet::new()), updated: AtomicBool::new(false) });
        for _ in 0..WORKERS {
            let me = me.clone();
            std::thread::spawn(move || me.work());
        }
        me
    }

    fn file(&self, domain: &str, ext: &str) -> PathBuf {
        let safe: String = domain.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-')).collect();
        self.dir.join(format!("{safe}.{ext}"))
    }

    /// The cached icon, if there is one.
    pub fn cached(&self, domain: &str) -> Option<PathBuf> {
        let p = self.file(domain, "png");
        p.exists().then_some(p)
    }

    /// Queues a download unless the icon is cached, known to be missing, or already on its way.
    pub fn request(&self, domain: &str) {
        if self.cached(domain).is_some() {
            return;
        }
        let none = self.file(domain, "none");
        if std::fs::metadata(&none).and_then(|m| m.modified()).is_ok_and(|t| t.elapsed().unwrap_or_default() < RETRY_AFTER) {
            return;
        }
        if self.pending.lock().unwrap().insert(domain.to_string()) {
            self.queue.lock().unwrap().push_back(domain.to_string());
            self.wake.notify_one();
        }
    }

    /// Whether downloads are still running (used when taking screenshots).
    pub fn busy(&self) -> bool {
        !self.pending.lock().unwrap().is_empty()
    }

    fn work(&self) {
        loop {
            let domain = {
                let mut q = self.queue.lock().unwrap();
                loop {
                    if let Some(d) = q.pop_front() {
                        break d;
                    }
                    q = self.wake.wait(q).unwrap();
                }
            };
            match fetch(&domain) {
                Some(png) if std::fs::write(self.file(&domain, "png"), &png).is_ok() => self.updated.store(true, Ordering::Relaxed),
                _ => {
                    let _ = std::fs::write(self.file(&domain, "none"), b"");
                }
            }
            self.pending.lock().unwrap().remove(&domain);
        }
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(8)))
        .user_agent("Mozilla/5.0 (compatible; Thundergoose)")
        .http_status_as_error(true)
        .build()
        .into()
}

fn download(agent: &ureq::Agent, url: &str) -> Option<Vec<u8>> {
    let mut resp = agent.get(url).call().ok()?;
    resp.body_mut().with_config().limit(MAX_DOWNLOAD).read_to_vec().ok()
}

/// Value of `name="…"` inside one tag.
fn attr<'a>(tag: &'a str, lower: &str, name: &str) -> Option<&'a str> {
    let mut from = 0;
    while let Some(i) = lower[from..].find(name) {
        let at = from + i;
        from = at + name.len();
        let before_ok = at == 0 || lower.as_bytes()[at - 1].is_ascii_whitespace();
        let rest = lower[from..].trim_start();
        if !before_ok || !rest.starts_with('=') {
            continue;
        }
        let value_start = tag.len() - rest.len() + 1;
        let value = tag[value_start..].trim_start();
        let start = tag.len() - value.len();
        return Some(match value.chars().next()? {
            q @ ('"' | '\'') => {
                let end = value[1..].find(q)? + 1;
                &tag[start + 1..start + end]
            }
            _ => {
                let end = value.find(|c: char| c.is_whitespace() || c == '>').unwrap_or(value.len());
                &tag[start..start + end]
            }
        });
    }
    None
}

/// Icon links of a page, best first.
fn icon_links(html: &str, origin: &str) -> Vec<String> {
    let lower = html.to_ascii_lowercase();
    let mut found: Vec<(i32, String)> = Vec::new();
    let mut from = 0;
    while let Some(i) = lower[from..].find("<link") {
        let start = from + i;
        let Some(len) = lower[start..].find('>') else { break };
        let (tag, tag_lower) = (&html[start..start + len], &lower[start..start + len]);
        from = start + len;
        let (Some(rel), Some(href)) = (attr(tag, tag_lower, "rel"), attr(tag, tag_lower, "href")) else { continue };
        let rel = rel.to_ascii_lowercase();
        if !rel.split_whitespace().any(|r| r == "icon" || r == "apple-touch-icon" || r == "apple-touch-icon-precomposed") {
            continue;
        }
        let href = href.replace("&amp;", "&");
        if href.ends_with(".svg") || href.starts_with("data:") {
            continue;
        }
        let size: i32 = attr(tag, tag_lower, "sizes").and_then(|s| s.split(['x', 'X']).next()?.parse().ok()).unwrap_or(if rel.contains("apple") { 180 } else { 32 });
        // Closest to 64 px from above is best; tiny icons last.
        let score = if size >= SIZE as i32 { 1000 - (size - SIZE as i32).min(900) } else { size };
        let url = if href.starts_with("http://") || href.starts_with("https://") {
            href
        } else if let Some(rest) = href.strip_prefix("//") {
            format!("https://{rest}")
        } else if href.starts_with('/') {
            format!("{origin}{href}")
        } else {
            format!("{origin}/{href}")
        };
        found.push((score, url));
    }
    found.sort_by_key(|(s, _)| -s);
    found.into_iter().map(|(_, u)| u).collect()
}

/// A decodable, not entirely blank image, as a 64 px PNG.
fn to_png(bytes: &[u8]) -> Option<Vec<u8>> {
    let img = image::load_from_memory(bytes).ok()?;
    if img.width() < 16 || img.height() < 16 {
        return None;
    }
    let img = if img.width() > SIZE || img.height() > SIZE { img.resize(SIZE, SIZE, image::imageops::FilterType::Lanczos3) } else { img };
    let rgba = img.to_rgba8();
    if rgba.pixels().all(|p| p[3] < 10) {
        return None;
    }
    let mut out = std::io::Cursor::new(Vec::new());
    rgba.write_to(&mut out, image::ImageFormat::Png).ok()?;
    Some(out.into_inner())
}

fn fetch(domain: &str) -> Option<Vec<u8>> {
    let agent = agent();
    for origin in [format!("https://{domain}"), format!("https://www.{domain}")] {
        let Some(page) = download(&agent, &format!("{origin}/")) else { continue };
        let html = String::from_utf8_lossy(&page);
        let mut urls = icon_links(&html, &origin);
        urls.push(format!("{origin}/favicon.ico"));
        for url in urls.iter().take(4) {
            if let Some(png) = download(&agent, url).as_deref().and_then(to_png) {
                return Some(png);
            }
        }
        return None;
    }
    download(&agent, &format!("https://{domain}/favicon.ico")).as_deref().and_then(to_png)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn providers() {
        assert!(domain_for("a@gmail.com").is_none());
        assert!(domain_for("a@yahoo.co.jp").is_none());
        assert!(domain_for("a@hotmail.fr").is_none());
        assert!(domain_for("a@gmx.de").is_none());
        assert_eq!(domain_for("noreply@notifications.github.com").as_deref(), Some("github.com"));
        assert_eq!(domain_for("x@cs.uni-example.de").as_deref(), Some("uni-example.de"));
    }

    #[test]
    fn finds_icon_links() {
        let html = r#"<head><LINK rel="icon" href="/fav-16.png" sizes="16x16"><link href='https://cdn.x.org/a.png' rel="apple-touch-icon"><link rel=stylesheet href=/s.css></head>"#;
        assert_eq!(icon_links(html, "https://x.org"), vec!["https://cdn.x.org/a.png".to_string(), "https://x.org/fav-16.png".to_string()]);
    }
}
