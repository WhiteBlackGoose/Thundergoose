//! Thundergoose on Android: the app is the `thundergoose` library; this only starts it.

#[cfg(target_os = "android")]
#[no_mangle]
fn android_main(app: thundergoose::AndroidApp) {
    thundergoose::run_android(app);
}
