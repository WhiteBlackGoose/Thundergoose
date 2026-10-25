//! `thundergoose --screenshots <dir>`: renders the demo data without a display, for the README.

use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, WindowAdapter, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, PhysicalSize, PlatformError, Rgb8Pixel};

use crate::{App, AppWindow};

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 800;
const SCALE: f32 = 2.0;

struct Headless {
    window: Rc<MinimalSoftwareWindow>,
}

impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.window.clone())
    }
}

/// Runs timers until icons and previews are in, plus a moment for scrolling to settle.
fn settle() {
    let start = Instant::now();
    let mut quiet_since: Option<Instant> = None;
    while start.elapsed() < Duration::from_secs(30) {
        slint::platform::update_timers_and_animations();
        std::thread::sleep(Duration::from_millis(15));
        if crate::app::busy() {
            quiet_since = None;
        } else if quiet_since.get_or_insert_with(Instant::now).elapsed() > Duration::from_millis(1200) {
            break;
        }
    }
}

fn save(window: &MinimalSoftwareWindow, path: &Path) {
    let (w, h) = ((WIDTH as f32 * SCALE) as u32, (HEIGHT as f32 * SCALE) as u32);
    let mut buf = vec![Rgb8Pixel { r: 0, g: 0, b: 0 }; (w * h) as usize];
    window.request_redraw();
    window.draw_if_needed(|r| {
        r.render(&mut buf, w as usize);
    });
    let bytes: Vec<u8> = buf.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
    match image::RgbImage::from_raw(w, h, bytes).map(|img| img.save(path)) {
        Some(Ok(())) => println!("{}", path.display()),
        _ => eprintln!("cannot write {}", path.display()),
    }
}

pub fn take(out: &Path) -> Result<(), PlatformError> {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Headless { window: window.clone() })).map_err(|e| PlatformError::Other(e.to_string()))?;
    window.dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: SCALE });
    window.set_size(PhysicalSize::new((WIDTH as f32 * SCALE) as u32, (HEIGHT as f32 * SCALE) as u32));
    let (ui, _timers): (AppWindow, _) = crate::app::start(true)?;
    ui.show()?;
    std::fs::create_dir_all(out).map_err(|e| PlatformError::Other(e.to_string()))?;
    let pointer = |x: f32, y: f32| window.dispatch_event(WindowEvent::PointerMoved { position: LogicalPosition::new(x, y) });
    let look = |theme: i32, accent: i32| crate::app::with_core(|c| c.set_look(theme, accent));
    let open = |key: &str| crate::app::with_core(|c| c.open_peer(key.to_string()));

    // Everything opened once first, so icons and previews are ready for every picture.
    for key in ["orders@trailhead-outfitters.example", "leo@park.example,maya@chen.example,priya@natarajan.example", "", "maya@chen.example"] {
        open(key);
        settle();
    }

    look(2, 0);
    open("maya@chen.example");
    pointer(180.0, 470.0);
    settle();
    save(&window, &out.join("chat-dark.png"));

    pointer(-10.0, -10.0);
    look(1, 0);
    open("leo@park.example,maya@chen.example,priya@natarajan.example");
    settle();
    save(&window, &out.join("group-light.png"));

    look(2, 2);
    open("");
    settle();
    save(&window, &out.join("yourself-dark.png"));

    look(2, 0);
    open("orders@trailhead-outfitters.example");
    settle();
    save(&window, &out.join("html-dark.png"));

    look(1, 3);
    crate::app::with_core(|c| c.set_tab(3));
    open("billing@secure-renewal.example");
    settle();
    save(&window, &out.join("spam-light.png"));
    crate::app::with_core(|c| c.set_tab(0));

    look(1, 1);
    open("notifications@github.com");
    ui.global::<App>().set_settings_open(true);
    settle();
    save(&window, &out.join("settings-light.png"));

    // First start on a computer with Thunderbird (not used in the README).
    if std::env::var_os("THUNDERGOOSE_SHOT_WELCOME").is_some() {
        crate::app::with_core(|c| c.forget_accounts_for_shot());
        ui.global::<App>().set_profile_path("/home/sam/.thunderbird/x1y2z3.default".into());
        settle();
        save(&window, &out.join("welcome.png"));
    }
    Ok(())
}
