//! Desktop notifications and the new-mail sound.

use std::path::{Path, PathBuf};

/// macOS bundle identifier (also in the app's Info.plist).
#[cfg(target_os = "macos")]
const BUNDLE_ID: &str = "io.github.whiteblackgoose.thundergoose";

/// The app icon as a file, for notification servers that want a path.
#[cfg(not(target_os = "android"))]
fn icon_file(data: &Path) -> Option<PathBuf> {
    let path = data.join("icon.png");
    if !path.exists() {
        std::fs::write(&path, include_bytes!("../packaging/thundergoose-256.png")).ok()?;
    }
    Some(path)
}

#[cfg(target_os = "android")]
mod android {
    use jni::objects::JValue;

    /// Asks for the notification permission (Android 13 and newer need it).
    pub fn request_permission() {
        let _ = crate::platform::imp::with_activity(|env, activity| {
            let sdk = env.get_static_field("android/os/Build$VERSION", "SDK_INT", "I")?.i()?;
            if sdk >= 33 {
                let perm = env.new_string("android.permission.POST_NOTIFICATIONS")?;
                let arr = env.new_object_array(1, "java/lang/String", &perm)?;
                env.call_method(activity, "requestPermissions", "([Ljava/lang/String;I)V", &[JValue::Object(&arr), JValue::Int(1)])?;
            }
            Ok(())
        });
    }

    /// A "New mail" notification; tapping it brings Thundergoose to the front.
    pub fn show(title: &str, body: &str) -> Result<(), String> {
        crate::platform::imp::with_activity(|env, activity| {
            let service = env.new_string("notification")?;
            let nm = env.call_method(activity, "getSystemService", "(Ljava/lang/String;)Ljava/lang/Object;", &[JValue::Object(&service)])?.l()?;
            let channel_id = env.new_string("mail")?;
            let channel_name = env.new_string("New mail")?;
            // IMPORTANCE_DEFAULT: makes a sound, shows in the status bar.
            let channel = env.new_object("android/app/NotificationChannel", "(Ljava/lang/String;Ljava/lang/CharSequence;I)V", &[JValue::Object(&channel_id), JValue::Object(&channel_name), JValue::Int(3)])?;
            env.call_method(&nm, "createNotificationChannel", "(Landroid/app/NotificationChannel;)V", &[JValue::Object(&channel)])?;
            let builder = env.new_object("android/app/Notification$Builder", "(Landroid/content/Context;Ljava/lang/String;)V", &[JValue::Object(activity), JValue::Object(&channel_id)])?;
            let icon = env.get_static_field("android/R$drawable", "sym_action_email", "I")?.i()?;
            let (t, b) = (env.new_string(title)?, env.new_string(body)?);
            env.call_method(&builder, "setSmallIcon", "(I)Landroid/app/Notification$Builder;", &[JValue::Int(icon)])?;
            env.call_method(&builder, "setContentTitle", "(Ljava/lang/CharSequence;)Landroid/app/Notification$Builder;", &[JValue::Object(&t)])?;
            env.call_method(&builder, "setContentText", "(Ljava/lang/CharSequence;)Landroid/app/Notification$Builder;", &[JValue::Object(&b)])?;
            env.call_method(&builder, "setAutoCancel", "(Z)Landroid/app/Notification$Builder;", &[JValue::Bool(1)])?;
            let pm = env.call_method(activity, "getPackageManager", "()Landroid/content/pm/PackageManager;", &[])?.l()?;
            let pkg = env.call_method(activity, "getPackageName", "()Ljava/lang/String;", &[])?.l()?;
            let launch = env.call_method(&pm, "getLaunchIntentForPackage", "(Ljava/lang/String;)Landroid/content/Intent;", &[JValue::Object(&pkg)])?.l()?;
            if !launch.is_null() {
                // FLAG_IMMUTABLE | FLAG_UPDATE_CURRENT
                let pending = env
                    .call_static_method(
                        "android/app/PendingIntent",
                        "getActivity",
                        "(Landroid/content/Context;ILandroid/content/Intent;I)Landroid/app/PendingIntent;",
                        &[JValue::Object(activity), JValue::Int(0), JValue::Object(&launch), JValue::Int(0x0400_0000 | 0x0800_0000)],
                    )?
                    .l()?;
                env.call_method(&builder, "setContentIntent", "(Landroid/app/PendingIntent;)Landroid/app/Notification$Builder;", &[JValue::Object(&pending)])?;
            }
            let notification = env.call_method(&builder, "build", "()Landroid/app/Notification;", &[])?.l()?;
            let id = (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() % i32::MAX as u128) as i32;
            env.call_method(&nm, "notify", "(ILandroid/app/Notification;)V", &[JValue::Int(id), JValue::Object(&notification)])?;
            Ok(())
        })
    }
}

#[cfg(target_os = "android")]
pub use android::request_permission;

#[cfg(target_os = "android")]
pub fn show(_data: &Path, title: &str, body: &str, _on_click: Option<Box<dyn FnOnce() + Send>>) {
    let (title, body) = (title.to_string(), body.to_string());
    std::thread::spawn(move || {
        if let Err(e) = android::show(&title, &body) {
            eprintln!("notification: {e}");
        }
    });
}

/// Shows a notification through the system's notification service (freedesktop on Linux,
/// Notification Center on macOS, toasts on Windows). `on_click` runs when it is clicked, where
/// the platform reports that.
#[cfg(not(target_os = "android"))]
pub fn show(data: &Path, title: &str, body: &str, on_click: Option<Box<dyn FnOnce() + Send>>) {
    let (title, body, icon) = (title.to_string(), body.to_string(), icon_file(data));
    std::thread::spawn(move || {
        #[cfg(target_os = "macos")]
        {
            static ONCE: std::sync::Once = std::sync::Once::new();
            ONCE.call_once(|| {
                let _ = notify_rust::set_application(BUNDLE_ID);
            });
        }
        let mut n = notify_rust::Notification::new();
        n.appname("Thundergoose").summary(&title).body(&body);
        // The installer registers this ID on the Start menu shortcut.
        #[cfg(target_os = "windows")]
        n.app_id("Thundergoose");
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            n.hint(notify_rust::Hint::DesktopEntry("thundergoose".into())).hint(notify_rust::Hint::Category("email.arrived".into()));
            match &icon {
                Some(p) => n.icon(&p.to_string_lossy()),
                None => n.icon("mail-unread"),
            };
            if on_click.is_some() {
                n.action("default", "Open");
            }
        }
        let _ = &icon;
        match n.show() {
            #[cfg(all(unix, not(target_os = "macos")))]
            Ok(handle) => {
                if let Some(f) = on_click {
                    handle.wait_for_action(|action| {
                        if action == "default" {
                            f();
                        }
                    });
                }
            }
            #[cfg(not(all(unix, not(target_os = "macos"))))]
            Ok(_) => drop(on_click),
            Err(e) => eprintln!("notification: {e}"),
        }
    });
}

/// A short two-note chime as 16-bit mono WAV.
fn chime() -> Vec<u8> {
    let rate = 44_100u32;
    let notes = [(880.0f32, 0.0f32), (1318.5, 0.11)];
    let len = (rate as f32 * 0.55) as usize;
    let mut samples = vec![0f32; len];
    for (freq, start) in notes {
        let s0 = (start * rate as f32) as usize;
        for (i, s) in samples.iter_mut().enumerate().skip(s0) {
            let t = (i - s0) as f32 / rate as f32;
            let attack = (t / 0.004).min(1.0);
            *s += 0.32 * attack * (-t * 9.0).exp() * (std::f32::consts::TAU * freq * t).sin();
        }
    }
    let mut wav = Vec::with_capacity(44 + len * 2);
    let data_len = (len * 2) as u32;
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&rate.to_le_bytes());
    wav.extend_from_slice(&(rate * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        wav.extend_from_slice(&((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16).to_le_bytes());
    }
    wav
}

fn chime_file(data: &Path) -> Option<PathBuf> {
    let path = data.join("chime.wav");
    if !path.exists() {
        std::fs::write(&path, chime()).ok()?;
    }
    Some(path)
}

/// Plays the chime with whatever player the system has. On Android the notification channel
/// plays the system's sound.
pub fn sound(data: &Path) {
    if cfg!(target_os = "android") {
        return;
    }
    let Some(path) = chime_file(data) else { return };
    std::thread::spawn(move || {
        let players: &[(&str, &[&str])] = if cfg!(target_os = "macos") {
            &[("afplay", &[])]
        } else if cfg!(target_os = "windows") {
            &[]
        } else {
            &[("pw-play", &[]), ("paplay", &[]), ("aplay", &["-q"])]
        };
        for (cmd, args) in players {
            let ok = std::process::Command::new(cmd).args(*args).arg(&path).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().is_ok_and(|s| s.success());
            if ok {
                return;
            }
        }
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            let script = format!("(New-Object Media.SoundPlayer '{}').PlaySync()", path.display());
            let _ = std::process::Command::new("powershell").args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", &script]).creation_flags(0x0800_0000).status();
        }
    });
}
