//! WordCraft on Android phones and tablets.
//!
//! The same [`wordcraft_ui_egui::WordApp`] as the desktop and web apps, run by eframe's
//! NativeActivity runner (wgpu: Vulkan, or OpenGL ES where Vulkan is missing). Android loads this
//! library (`libwordcraft_android.so`) into `WordCraftActivity`, a small `NativeActivity` subclass
//! (`android/java/…`) that adds what NDK code can't do alone: the system file pickers, sharing the
//! clipboard, printing and the on-screen keyboard's text.
//!
//! Differences from the desktop app, much like the browser's (`apps/wordcraft-web`):
//! - Open and Insert › Pictures use Android's document picker; picked files arrive through the
//!   app's inbox ([`mailbox`]). Opening a `.docx` from a file manager or another app's Share/Open
//!   with does the same.
//! - Save and Export go through Android's save picker (a later save to the same name writes back
//!   to the file it came from without asking again, while the app keeps permission for it).
//! - The on-screen keyboard types into the activity's own text field, which passes text and keys
//!   through the inbox in order ([`keys`]); the app keeps clear of the status bar, navigation bar
//!   and keyboard (`insets`).
//! - Fingers: tap clicks, swipe scrolls, long press then drag selects, two fingers pinch-zoom,
//!   and a two-finger tap opens the context menu ([`touch`]). The system Back button is Escape.
//! - Settings, added fonts, the user dictionary and recovery files live in the app's private
//!   storage.
//!
//! Build the APK with `apps/wordcraft-android/build-apk.sh` (see the README beside it).
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod keys;
pub mod mailbox;
pub mod saving;
pub mod touch;

#[cfg(target_os = "android")]
mod bridge;
#[cfg(target_os = "android")]
mod shell;

/// The entry point android-activity's glue calls on its own thread once the activity starts.
// SAFETY: `android_main` must be exported unmangled: the android-activity glue looks it up by
// this exact name. Nothing else in the process defines it.
#[cfg(target_os = "android")]
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
fn android_main(app: winit::platform::android::activity::AndroidApp) {
    shell::run(app);
}
