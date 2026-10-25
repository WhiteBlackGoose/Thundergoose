#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod autoconfig;
mod config;
mod creds;
mod demo;
mod favicon;
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
        Some("--version") => {
            println!("thundergoose {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        _ => app::run(false),
    }
}
