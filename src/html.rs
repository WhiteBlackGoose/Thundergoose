//! HTML mail, rendered in the conversation with Blitz (a Rust HTML/CSS engine) into a picture,
//! plus where its links are. Images embedded in the mail (`cid:`, `data:`) always load; remote
//! ones (tracking pixels, mostly) only when the user asks.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine;
use blitz_dom::{local_name, BaseDocument, DocumentConfig};
use blitz_html::HtmlDocument;
use blitz_traits::net::{Bytes, NetHandler, NetProvider, Request};
use blitz_traits::shell::{ColorScheme, Viewport};
use slint::{Rgba8Pixel, SharedPixelBuffer};

/// Mail is laid out at the width newsletters are designed for.
pub const CSS_WIDTH: f32 = 620.0;
/// A collapsed mail is cut here; "Show full message" renders everything.
const CUT_HEIGHT: f32 = 1500.0;
const MAX_HEIGHT: f32 = 12000.0;
const REMOTE_TIMEOUT: Duration = Duration::from_secs(8);
const REMOTE_MAX: u64 = 5 * 1024 * 1024;

pub struct Link {
    /// x, y, width, height in CSS pixels.
    pub rect: (f32, f32, f32, f32),
    pub url: String,
}

pub struct Rendered {
    pub pixels: SharedPixelBuffer<Rgba8Pixel>,
    pub links: Vec<Link>,
    pub css_height: f32,
    /// The mail asked for remote content that wasn't loaded.
    pub blocked: bool,
    /// Quoted history was removed.
    pub quoted: bool,
    /// The bottom was cut off.
    pub cut: bool,
}

/// Markers of quoted history in HTML from common mail programs.
const QUOTE_MARKERS: &[&str] = &["class=\"gmail_quote", "id=\"divrplyfwdmsg\"", "id=\"appendonsend\"", "<blockquote type=\"cite\"", "class=\"moz-cite-prefix\"", "class=\"yahoo_quoted\"", "id=\"mail-editor-reference-message-container\""];

/// Whether the mail's own part (before any quoted history) has layout worth showing as HTML:
/// tables, pictures, headings, colors. Plain paragraphs read better as text.
pub fn is_rich(html: &str) -> bool {
    let lower = html.to_ascii_lowercase();
    let own = QUOTE_MARKERS.iter().filter_map(|m| lower.find(m)).min().map_or(lower.as_str(), |i| &lower[..i]);
    ["<table", "<img", "background", "<h1", "<h2", "<center", "<button", "<hr"].iter().any(|t| own.contains(t))
}

struct Provider {
    inline: HashMap<String, Vec<u8>>,
    remote: bool,
    blocked: AtomicBool,
    pending: Arc<AtomicUsize>,
}

fn data_url(url: &str) -> Option<Vec<u8>> {
    let (meta, data) = url.strip_prefix("data:")?.split_once(',')?;
    if meta.ends_with(";base64") {
        base64::engine::general_purpose::STANDARD.decode(data.trim()).ok()
    } else {
        Some(percent_decode(data))
    }
}

fn percent_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

impl NetProvider for Provider {
    fn fetch(&self, _doc: usize, request: Request, handler: Box<dyn NetHandler>) {
        let url = request.url.to_string();
        let bytes = match request.url.scheme() {
            "cid" => {
                let id = percent_decode(request.url.path());
                self.inline.get(String::from_utf8_lossy(&id).trim_matches(['<', '>'])).cloned()
            }
            "data" => data_url(&url),
            "http" | "https" if self.remote => {
                self.pending.fetch_add(1, Ordering::SeqCst);
                let pending = self.pending.clone();
                std::thread::spawn(move || {
                    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(REMOTE_TIMEOUT)).http_status_as_error(true).build().into();
                    let body = agent.get(&url).call().ok().and_then(|mut r| r.body_mut().with_config().limit(REMOTE_MAX).read_to_vec().ok());
                    handler.bytes(url, Bytes::from(body.unwrap_or_default()));
                    pending.fetch_sub(1, Ordering::SeqCst);
                });
                return;
            }
            _ => {
                self.blocked.store(true, Ordering::Relaxed);
                None
            }
        };
        handler.bytes(url, Bytes::from(bytes.unwrap_or_default()));
    }
}

/// Removes quoted history from the document; returns whether there was any.
fn strip_quotes(doc: &mut BaseDocument) -> bool {
    let mut doomed = Vec::new();
    for sel in [".gmail_quote", "blockquote[type=cite]", ".moz-cite-prefix", ".yahoo_quoted", "#mail-editor-reference-message-container"] {
        if let Ok(ids) = doc.query_selector_all(sel) {
            doomed.extend(ids);
        }
    }
    // Outlook: the reply header and everything after it, plus the rule above it.
    for sel in ["#divRplyFwdMsg", "#appendonsend"] {
        if let Ok(Some(id)) = doc.query_selector(sel) {
            if let Some(parent) = doc.get_node(id).and_then(|n| n.parent).and_then(|p| doc.get_node(p)) {
                let children: Vec<_> = parent.children.iter().copied().collect();
                if let Some(pos) = children.iter().position(|c| *c == id) {
                    let start = if pos > 0 && doc.get_node(children[pos - 1]).and_then(|n| n.element_data()).is_some_and(|e| e.name.local == local_name!("hr")) { pos - 1 } else { pos };
                    doomed.extend(children[start..].iter().copied());
                }
            }
        }
    }
    if doomed.is_empty() {
        return false;
    }
    let mut m = doc.mutate();
    for id in doomed {
        m.remove_node(id);
    }
    true
}

/// Where the links are, by hit-testing a grid inside each link's nearest laid-out box.
fn links(doc: &BaseDocument, max_y: f32) -> Vec<Link> {
    const STEP: f32 = 4.0;
    let mut out = Vec::new();
    let Ok(ids) = doc.query_selector_all("a[href]") else { return out };
    for id in ids.iter().copied() {
        let Some(node) = doc.get_node(id) else { continue };
        let url = node.attr(local_name!("href")).unwrap_or("").trim().to_string();
        if !(url.starts_with("http://") || url.starts_with("https://") || url.starts_with("mailto:")) {
            continue;
        }
        let mut b = id;
        while let Some(n) = doc.get_node(b) {
            let s = n.final_layout().size;
            if s.width > 0.0 && s.height > 0.0 {
                break;
            }
            match n.parent {
                Some(p) => b = p,
                None => break,
            }
        }
        let Some(block) = doc.get_node(b) else { continue };
        let p = block.absolute_position(0.0, 0.0);
        let size = block.final_layout().size;
        let mut y = p.y + STEP / 2.0;
        while y < (p.y + size.height).min(max_y) {
            let mut run: Option<f32> = None;
            let mut x = p.x + STEP / 2.0;
            while x < p.x + size.width + STEP {
                let inside = x < p.x + size.width
                    && doc.hit(x, y).is_some_and(|h| {
                        let mut cur = Some(h.node_id);
                        while let Some(c) = cur {
                            if c == id {
                                return true;
                            }
                            cur = doc.get_node(c).and_then(|n| n.parent);
                        }
                        false
                    });
                match (inside, run) {
                    (true, None) => run = Some(x),
                    (false, Some(x0)) => {
                        out.push(Link { rect: (x0 - STEP / 2.0, y - STEP / 2.0, x - x0, STEP), url: url.clone() });
                        run = None;
                    }
                    _ => {}
                }
                x += STEP;
            }
            y += STEP;
        }
    }
    out
}

/// Renders `html` at `scale` (physical pixels per CSS pixel). `full` keeps quoted history and
/// doesn't cut long mail.
pub fn render(html: &str, inline: HashMap<String, Vec<u8>>, remote: bool, scale: f32, full: bool) -> Option<Rendered> {
    let pending = Arc::new(AtomicUsize::new(0));
    let provider = Arc::new(Provider { inline, remote, blocked: AtomicBool::new(false), pending: pending.clone() });
    let width = (CSS_WIDTH * scale).round() as u32;
    let mut doc = HtmlDocument::from_html(
        html,
        DocumentConfig { viewport: Some(Viewport::new(width, 800, scale, ColorScheme::Light)), net_provider: Some(provider.clone()), ..Default::default() },
    );
    let quoted = if full { false } else { strip_quotes(&mut doc) };
    doc.resolve(0.0);
    let start = Instant::now();
    while pending.load(Ordering::SeqCst) > 0 && start.elapsed() < REMOTE_TIMEOUT + Duration::from_secs(1) {
        std::thread::sleep(Duration::from_millis(25));
        doc.resolve(0.0);
    }
    doc.resolve(0.0);
    let natural = doc.root_element().final_layout().size.height.max(1.0);
    let limit = if full { MAX_HEIGHT } else { CUT_HEIGHT };
    let css_height = natural.min(limit);
    let height = (css_height * scale).ceil() as u32;
    let links = links(&doc, css_height);
    let rgba = anyrender::render_to_buffer::<anyrender_vello_cpu::VelloCpuImageRenderer, _>(|scene| blitz_paint::paint_scene(scene, &mut doc, scale as f64, width, height, 0, 0), width, height);
    // Mail without a background of its own is meant to be on white.
    let mut pixels = SharedPixelBuffer::<Rgba8Pixel>::new(width, height);
    for (dst, src) in pixels.make_mut_slice().iter_mut().zip(rgba.chunks_exact(4)) {
        let a = src[3] as u16;
        let over = |c: u8| (c as u16 + (255 - a)).min(255) as u8;
        *dst = Rgba8Pixel { r: over(src[0]), g: over(src[1]), b: over(src[2]), a: 255 };
    }
    Some(Rendered { pixels, links, css_height, blocked: provider.blocked.load(Ordering::Relaxed), quoted, cut: natural > limit })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rich_or_not() {
        assert!(is_rich("<html><body><table><tr><td>Hi</td></tr></table></body></html>"));
        assert!(!is_rich("<div dir=\"ltr\">Thanks!<br></div><div class=\"gmail_quote\"><table></table></div>"));
    }

    #[test]
    fn data_urls() {
        assert_eq!(data_url("data:text/plain;base64,aGk=").as_deref(), Some(&b"hi"[..]));
        assert_eq!(data_url("data:,a%20b").as_deref(), Some(&b"a b"[..]));
    }
}
