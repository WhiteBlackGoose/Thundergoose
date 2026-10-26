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
/// The phone pictures: a typical phone in logical pixels, and its pixel density.
const PHONE: (u32, u32, f32) = (400, 860, 2.5);

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
    let size = window.size();
    let (w, h) = (size.width, size.height);
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

    // Sending, and suggestions while typing an address (not used in the README).
    if std::env::var_os("THUNDERGOOSE_SHOT_SEND").is_some() {
        ui.global::<App>().set_settings_open(false);
        look(2, 0);
        open("maya@chen.example");
        settle();
        ui.global::<App>().set_draft_text("Sounds great, see you Saturday!\n\nSam".into());
        ui.global::<App>().invoke_send();
        for _ in 0..20 {
            slint::platform::update_timers_and_animations();
            std::thread::sleep(Duration::from_millis(15));
        }
        save(&window, &out.join("sending.png"));
        settle();
        std::thread::sleep(Duration::from_millis(1800));
        settle();
        save(&window, &out.join("sent.png"));
        ui.global::<App>().invoke_start_new_chat();
        settle();
        for ch in ["p", "r"] {
            window.dispatch_event(WindowEvent::KeyPressed { text: ch.into() });
            window.dispatch_event(WindowEvent::KeyReleased { text: ch.into() });
            slint::platform::update_timers_and_animations();
        }
        settle();
        save(&window, &out.join("suggest.png"));
    }

    // Any conversation, by key (not used in the README).
    if let Ok(key) = std::env::var("THUNDERGOOSE_SHOT_OPEN") {
        ui.global::<App>().set_settings_open(false);
        look(2, 0);
        open(&key);
        settle();
        save(&window, &out.join("open.png"));
    }

    // Selecting text in a rendered HTML mail (not used in the README).
    if std::env::var_os("THUNDERGOOSE_SHOT_SELECT").is_some() {
        use slint::platform::PointerEventButton;
        ui.global::<App>().set_settings_open(false);
        look(2, 0);
        open("orders@trailhead-outfitters.example");
        settle();
        let (a, b) = (LogicalPosition::new(470.0, 207.0), LogicalPosition::new(760.0, 232.0));
        window.dispatch_event(WindowEvent::PointerMoved { position: a });
        window.dispatch_event(WindowEvent::PointerPressed { position: a, button: PointerEventButton::Left });
        for i in 1..=10 {
            let t = i as f32 / 10.0;
            window.dispatch_event(WindowEvent::PointerMoved { position: LogicalPosition::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t) });
            slint::platform::update_timers_and_animations();
            std::thread::sleep(Duration::from_millis(30));
        }
        window.dispatch_event(WindowEvent::PointerReleased { position: b, button: PointerEventButton::Left });
        settle();
        save(&window, &out.join("select.png"));
    }

    // The tag color picker stays open while sync status keeps updating (not used in the README).
    if std::env::var_os("THUNDERGOOSE_SHOT_PICKER").is_some() {
        let click = |x: f32, y: f32| {
            use slint::platform::PointerEventButton;
            let position = LogicalPosition::new(x, y);
            window.dispatch_event(WindowEvent::PointerMoved { position });
            window.dispatch_event(WindowEvent::PointerPressed { position, button: PointerEventButton::Left });
            window.dispatch_event(WindowEvent::PointerReleased { position, button: PointerEventButton::Left });
        };
        click(384.0, 184.0);
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(4) {
            slint::platform::update_timers_and_animations();
            std::thread::sleep(Duration::from_millis(15));
        }
        save(&window, &out.join("picker.png"));
    }

    // The narrow, phone layout: list, drawer, conversation.
    {
        ui.global::<App>().set_settings_open(false);
        let (w, h, scale) = PHONE;
        window.dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: scale });
        window.set_size(PhysicalSize::new((w as f32 * scale) as u32, (h as f32 * scale) as u32));
        look(2, 0);
        crate::app::with_core(|c| c.set_tab(0));
        ui.global::<App>().invoke_go_back();
        settle();
        save(&window, &out.join("phone-list.png"));
        ui.global::<App>().set_drawer_open(true);
        settle();
        save(&window, &out.join("phone-drawer.png"));
        ui.global::<App>().set_drawer_open(false);
        open("maya@chen.example");
        settle();
        save(&window, &out.join("phone-chat.png"));
        open("orders@trailhead-outfitters.example");
        settle();
        save(&window, &out.join("phone-html.png"));
        // Settings with the account form at its server step (checks that nothing overflows).
        let app = ui.global::<App>();
        app.set_settings_open(true);
        app.set_add_open(true);
        app.set_add_step(1);
        app.set_add_email("sam@rivera.example".into());
        app.set_add_imap_host("imap.rivera.example".into());
        app.set_add_imap_port("993".into());
        app.set_add_smtp_host("smtp.rivera.example".into());
        app.set_add_smtp_port("465".into());
        settle();
        save(&window, &out.join("phone-settings.png"));
        app.set_add_open(false);
        app.set_settings_open(false);
    }

    // First start on a computer with Thunderbird (not used in the README).
    if std::env::var_os("THUNDERGOOSE_SHOT_WELCOME").is_some() {
        crate::app::with_core(|c| c.forget_accounts_for_shot());
        ui.global::<App>().set_profile_path("/home/sam/.thunderbird/x1y2z3.default".into());
        settle();
        save(&window, &out.join("welcome.png"));
    }
    Ok(())
}
