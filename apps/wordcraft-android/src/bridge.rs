//! Calls into `WordCraftActivity` (Java).
//!
//! Every call is a `void` method taking strings, numbers or a flag: it starts something on Java's
//! UI thread and returns at once. Answers come back as files in the inbox ([`crate::mailbox`]),
//! so nothing here waits for Java or receives Java objects.
//!
//! The activity comes from android-activity's `AndroidApp` (`activity_as_ptr`: a global reference
//! to the `WordCraftActivity` instance), kept by [`init`]. Not `ndk_context`'s context: that is
//! the *Application*, which has none of these methods.
#![allow(unsafe_code)]

use std::sync::OnceLock;

use jni::objects::{JObject, JValue};
use jni::{Env, JavaVM, jni_sig, jni_str};
use winit::platform::android::activity::AndroidApp;

static APP: OnceLock<AndroidApp> = OnceLock::new();

/// Keep the app handle for the calls below. Called once, before eframe starts.
pub fn init(app: &AndroidApp) {
    let _ = APP.set(app.clone());
}

/// Run `f` with this thread attached to the VM and the activity object.
fn with_activity<T>(f: impl FnOnce(&mut Env, &JObject) -> jni::errors::Result<T>) -> Result<T, String> {
    let app = APP.get().ok_or("the Android activity is not available yet")?;
    let (vm, activity) = (app.vm_as_ptr(), app.activity_as_ptr());
    if vm.is_null() || activity.is_null() {
        return Err("the Android activity is not available".into());
    }
    // SAFETY: `vm_as_ptr` is the process's `JavaVM`, valid for the life of the process.
    let vm = unsafe { JavaVM::from_raw(vm.cast()) };
    vm.attach_current_thread(|env| {
        // SAFETY: `activity_as_ptr` is a global reference to the activity, owned by
        // android-activity and valid while the activity exists. It is only borrowed here (a
        // `JObject` never deletes the reference it wraps), within this call.
        let activity = unsafe { JObject::from_raw(env, activity.cast()) };
        f(env, &activity)
    })
    .map_err(|e: jni::errors::Error| e.to_string())
}

/// `openDocument(String purpose)`: the system picker for a document (`document`), a picture
/// (`picture`), fonts (`font`, several at once) or a recipient list (`recipients`).
pub fn open(purpose: &str) -> Result<(), String> {
    with_activity(|env, activity| {
        let purpose = env.new_string(purpose)?;
        env.call_method(activity, jni_str!("openDocument"), jni_sig!("(Ljava/lang/String;)V"), &[JValue::Object(&purpose)])?;
        Ok(())
    })
}

/// `saveDocument(long ticket, String path, String name, String mime, boolean writeBack)`: save
/// the file at `path` (written by the app, in its private outbox) as `name`: with `write_back`,
/// over the file of that name the document came from when the app may still write there;
/// otherwise (or failing that) where the user picks.
pub fn save(ticket: u64, path: &str, name: &str, mime: &str, write_back: bool) -> Result<(), String> {
    with_activity(|env, activity| {
        let path = env.new_string(path)?;
        let name = env.new_string(name)?;
        let mime = env.new_string(mime)?;
        env.call_method(
            activity,
            jni_str!("saveDocument"),
            jni_sig!("(JLjava/lang/String;Ljava/lang/String;Ljava/lang/String;Z)V"),
            &[
                JValue::Long(i64::try_from(ticket).unwrap_or(i64::MAX)),
                JValue::Object(&path),
                JValue::Object(&name),
                JValue::Object(&mime),
                JValue::Bool(write_back),
            ],
        )?;
        Ok(())
    })
}

/// `printPdf(String path, String name)`: Android's print dialog for the PDF at `path`.
pub fn print(path: &str, name: &str) -> Result<(), String> {
    with_activity(|env, activity| {
        let path = env.new_string(path)?;
        let name = env.new_string(name)?;
        env.call_method(
            activity,
            jni_str!("printPdf"),
            jni_sig!("(Ljava/lang/String;Ljava/lang/String;)V"),
            &[JValue::Object(&path), JValue::Object(&name)],
        )?;
        Ok(())
    })
}

/// `setClipboardText(String text)`: put a copy on the system clipboard, for other apps.
pub fn set_clipboard(text: &str) -> Result<(), String> {
    with_activity(|env, activity| {
        let text = env.new_string(text)?;
        env.call_method(activity, jni_str!("setClipboardText"), jni_sig!("(Ljava/lang/String;)V"), &[JValue::Object(&text)])?;
        Ok(())
    })
}

/// `setKeyboardVisible(boolean)`: show or hide the on-screen keyboard.
pub fn keyboard(visible: bool) -> Result<(), String> {
    with_activity(|env, activity| {
        env.call_method(activity, jni_str!("setKeyboardVisible"), jni_sig!("(Z)V"), &[JValue::Bool(visible)])?;
        Ok(())
    })
}

/// `setLightSystemBars(boolean)`: dark status and navigation bar icons over a light app, light
/// icons over a dark one.
pub fn light_system_bars(light: bool) -> Result<(), String> {
    with_activity(|env, activity| {
        env.call_method(activity, jni_str!("setLightSystemBars"), jni_sig!("(Z)V"), &[JValue::Bool(light)])?;
        Ok(())
    })
}
