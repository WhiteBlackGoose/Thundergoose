//! Thundergoose: email as conversations with people. This library is the whole app; the
//! desktop binary (`src/main.rs`) and the Android app (`android/`) only start it.

mod app;
mod autoconfig;
mod config;
mod creds;
mod demo;
mod favicon;
mod html;
mod imapc;
mod notify;
mod platform;
mod nss;
mod oauth;
mod parse;
mod send;
mod shots;
mod store;
mod sync;
mod tb;

slint::include_modules!();

/// The desktop program: the app, or one of the command-line tools.
pub fn cli() -> Result<(), slint::PlatformError> {
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
            let (_, r) = html::Page::load(&html, Default::default(), false, 2.0, true, html::CSS_WIDTH);
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

/// The Android app's entry: called from `android_main` in the `android/` crate.
#[cfg(target_os = "android")]
pub fn run_android(app: slint::android::AndroidApp) {
    // Mail, settings and secrets live in the app's private storage.
    if let Some(dir) = app.internal_data_path() {
        std::env::set_var("THUNDERGOOSE_HOME", dir);
    }
    slint::android::init(app).expect("Slint's Android backend");
    notify::request_permission();
    if let Err(e) = app::run(false) {
        eprintln!("thundergoose: {e}");
    }
}

#[cfg(target_os = "android")]
pub use slint::android::AndroidApp;
