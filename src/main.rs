#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod autoconfig;
mod config;
mod creds;
mod demo;
mod favicon;
mod html;
mod imapc;
mod notify;
mod nss;
mod oauth;
mod parse;
mod send;
mod shots;
mod store;
mod sync;
mod tb;

slint::include_modules!();

fn main() -> Result<(), slint::PlatformError> {
    match std::env::args().nth(1).as_deref() {
        Some("--check") => {
            app::check();
            Ok(())
        }
        Some("--detect") => {
            let email = std::env::args().nth(2).unwrap_or_default();
            let d = autoconfig::detect(&email);
            println!("source: {}\noauth:  {:?}\nimap:   {:?}\nsmtp:   {:?}", d.source, d.oauth, d.imap, d.smtp);
            Ok(())
        }
        Some("--demo") => {
            demo::prepare();
            app::run(true)
        }
        Some("--screenshots") => {
            let out = std::path::PathBuf::from(std::env::args().nth(2).unwrap_or_else(|| "docs".into()));
            demo::prepare();
            shots::take(&out)
        }
        Some("--notify-test") => {
            let paths = config::Paths::new();
            notify::show(&paths.data, "Thundergoose", "Notifications work. New mail will show up like this.", None);
            notify::sound(&paths.data);
            std::thread::sleep(std::time::Duration::from_secs(2));
            Ok(())
        }
        Some("--render-html") => {
            // Developer aid: how an HTML file comes out in a conversation.
            let (input, output) = (std::env::args().nth(2).unwrap_or_default(), std::env::args().nth(3).unwrap_or_else(|| "mail.png".into()));
            let html = std::fs::read_to_string(&input).map_err(|e| slint::PlatformError::Other(format!("{input}: {e}")))?;
            let (_, r) = html::Page::load(&html, Default::default(), false, 2.0, true);
            let (w, h) = (r.pixels.width(), r.pixels.height());
            let bytes: Vec<u8> = r.pixels.as_slice().iter().flat_map(|p| [p.r, p.g, p.b]).collect();
            image::RgbImage::from_raw(w, h, bytes).map(|i| i.save(&output));
            println!("{output}: {w}x{h}, {} links, remote content {}", r.links.len(), if r.blocked { "blocked" } else { "none" });
            Ok(())
        }
        Some("--version") => {
            println!("thundergoose {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        _ => app::run(false),
    }
}
