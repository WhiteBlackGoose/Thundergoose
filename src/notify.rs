//! Desktop notifications and the new-mail sound.

use std::path::{Path, PathBuf};

pub fn show(title: &str, body: &str) {
    let (title, body) = (title.to_string(), body.to_string());
    std::thread::spawn(move || {
        let mut n = notify_rust::Notification::new();
        n.appname("Thundergoose").summary(&title).body(&body);
        #[cfg(all(unix, not(target_os = "macos")))]
        n.icon("mail-unread").hint(notify_rust::Hint::Category("email.arrived".into()));
        if let Err(e) = n.show() {
            eprintln!("notification: {e}");
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

/// Plays the chime with whatever player the system has.
pub fn sound(data: &Path) {
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
