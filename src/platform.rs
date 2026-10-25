//! What differs between desktop and Android: opening links and files, the clipboard, file
//! dialogs. On Android these go through the Java framework (JNI).

use std::path::{Path, PathBuf};

#[cfg(not(target_os = "android"))]
mod imp {
    use super::*;

    pub fn open(target: &str) -> Result<(), String> {
        open::that_detached(target).map_err(|e| e.to_string())
    }

    pub fn open_file(path: &Path) -> Result<(), String> {
        open::that_detached(path).map_err(|e| e.to_string())
    }

    pub fn copy(text: &str) -> Result<(), String> {
        arboard::Clipboard::new().and_then(|mut cb| cb.set_text(text.to_string())).map_err(|e| e.to_string())
    }

    pub fn pick_files() -> Option<Vec<PathBuf>> {
        rfd::FileDialog::new().pick_files()
    }

    pub fn save_as(name: &str) -> Option<PathBuf> {
        rfd::FileDialog::new().set_file_name(name).save_file()
    }

    pub fn data_dir() -> Option<PathBuf> {
        directories::ProjectDirs::from("", "", "Thundergoose").map(|d| d.data_dir().to_path_buf())
    }

    pub fn home_dir() -> Option<PathBuf> {
        directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf())
    }
}

#[cfg(target_os = "android")]
pub(crate) mod imp {
    use super::*;
    use jni::objects::{JObject, JValue};
    use jni::JNIEnv;

    /// Runs `f` with the JVM attached to this thread and the app's Activity.
    pub fn with_activity<T>(f: impl FnOnce(&mut JNIEnv, &JObject) -> jni::errors::Result<T>) -> Result<T, String> {
        let ctx = ndk_context::android_context();
        let vm = unsafe { jni::JavaVM::from_raw(ctx.vm().cast()) }.map_err(|e| e.to_string())?;
        let mut env = vm.attach_current_thread().map_err(|e| e.to_string())?;
        let activity = unsafe { JObject::from_raw(ctx.context().cast()) };
        let result = f(&mut env, &activity);
        if env.exception_check().unwrap_or(false) {
            let _ = env.exception_describe();
            let _ = env.exception_clear();
        }
        result.map_err(|e| e.to_string())
    }

    pub fn open(target: &str) -> Result<(), String> {
        with_activity(|env, activity| {
            let s = env.new_string(target)?;
            let uri = env.call_static_method("android/net/Uri", "parse", "(Ljava/lang/String;)Landroid/net/Uri;", &[JValue::Object(&s)])?.l()?;
            let action = env.new_string("android.intent.action.VIEW")?;
            let intent = env.new_object("android/content/Intent", "(Ljava/lang/String;Landroid/net/Uri;)V", &[JValue::Object(&action), JValue::Object(&uri)])?;
            // FLAG_ACTIVITY_NEW_TASK
            env.call_method(&intent, "addFlags", "(I)Landroid/content/Intent;", &[JValue::Int(0x1000_0000)])?;
            env.call_method(activity, "startActivity", "(Landroid/content/Intent;)V", &[JValue::Object(&intent)])?;
            Ok(())
        })
    }

    pub fn open_file(_path: &Path) -> Result<(), String> {
        Err("opening attachments isn't available on Android yet".into())
    }

    pub fn copy(text: &str) -> Result<(), String> {
        with_activity(|env, activity| {
            let name = env.new_string("clipboard")?;
            let cm = env.call_method(activity, "getSystemService", "(Ljava/lang/String;)Ljava/lang/Object;", &[JValue::Object(&name)])?.l()?;
            let label = env.new_string("Thundergoose")?;
            let s = env.new_string(text)?;
            let clip = env
                .call_static_method("android/content/ClipData", "newPlainText", "(Ljava/lang/CharSequence;Ljava/lang/CharSequence;)Landroid/content/ClipData;", &[JValue::Object(&label), JValue::Object(&s)])?
                .l()?;
            env.call_method(&cm, "setPrimaryClip", "(Landroid/content/ClipData;)V", &[JValue::Object(&clip)])?;
            Ok(())
        })
    }

    pub fn pick_files() -> Option<Vec<PathBuf>> {
        None
    }

    pub fn save_as(_name: &str) -> Option<PathBuf> {
        None
    }

    pub fn data_dir() -> Option<PathBuf> {
        None
    }

    pub fn home_dir() -> Option<PathBuf> {
        None
    }
}

pub use imp::{copy, data_dir, home_dir, open, open_file, pick_files, save_as};

/// Whether the platform has file dialogs (attach, save as).
pub const FILE_DIALOGS: bool = cfg!(not(target_os = "android"));
