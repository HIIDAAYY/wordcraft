//! Calls into `WordCraftActivity` (Java).
//!
//! Every call is a `void` method taking strings, numbers or a flag: it starts something on Java's
//! UI thread and returns at once. Answers come back as files in the inbox ([`crate::mailbox`]),
//! so nothing here waits for Java or receives Java objects.
//!
//! The activity is reached through `ndk_context`, which android-activity fills in before
//! `android_main` runs: the process's `JavaVM` and a global reference to the activity.
#![allow(unsafe_code)]

use jni::objects::{JObject, JValue};
use jni::{Env, JavaVM, jni_sig, jni_str};

/// Run `f` with this thread attached to the VM and the activity object.
fn with_activity<T>(f: impl FnOnce(&mut Env, &JObject) -> jni::errors::Result<T>) -> Result<T, String> {
    let ctx = ndk_context::android_context();
    if ctx.vm().is_null() || ctx.context().is_null() {
        return Err("the Android activity is not available".into());
    }
    // SAFETY: `ndk_context` holds the process's `JavaVM` pointer, set by android-activity before
    // `android_main` and valid for the life of the process.
    let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) };
    vm.attach_current_thread(|env| {
        // SAFETY: `ctx.context()` is a global reference to the activity, owned by android-activity
        // and valid while the activity exists. It is only borrowed here (a `JObject` never deletes
        // the reference it wraps), within this call.
        let activity = unsafe { JObject::from_raw(env, ctx.context().cast()) };
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

/// `saveDocument(long ticket, String path, String name, String mime)`: ask where to save the
/// file at `path` (written by the app, in its private outbox) as `name`, then copy it there.
pub fn save(ticket: u64, path: &str, name: &str, mime: &str) -> Result<(), String> {
    with_activity(|env, activity| {
        let path = env.new_string(path)?;
        let name = env.new_string(name)?;
        let mime = env.new_string(mime)?;
        env.call_method(
            activity,
            jni_str!("saveDocument"),
            jni_sig!("(JLjava/lang/String;Ljava/lang/String;Ljava/lang/String;)V"),
            &[JValue::Long(i64::try_from(ticket).unwrap_or(i64::MAX)), JValue::Object(&path), JValue::Object(&name), JValue::Object(&mime)],
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
