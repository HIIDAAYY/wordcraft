# WordCraft for Android

WordCraft on Android tablets and phones: the same app as on the desktop (the ribbon, styles,
tables, track changes, `.docx` in and out), running natively. It is not a web page in a wrapper.

> **Community port, early preview.** WordCraft itself is still alpha. Keep copies of documents
> that matter.

## Install

Download `WordCraft-<version>-arm64.apk` from this repository's **Actions** run (artifact
*WordCraft-android-arm64*) or **Releases**, open it on the tablet and allow the install when
Android asks. Needs Android 8.0 or newer on a 64-bit ARM device (nearly every tablet and phone
from the last several years).

## Using it with fingers

| Gesture | Does |
|---|---|
| Tap | Click: place the caret, press a button |
| Double tap | Select a word |
| Swipe | Scroll the page (or a list, or the ribbon's galleries); a flick keeps coasting |
| Long press, then drag | Select text |
| Two fingers apart / together | Zoom |
| Two-finger tap | Right-click (the context menu) |
| Back button | Escape: closes a menu, a dialog or File view |

The on-screen keyboard appears when you tap the page or a text box, and goes away when nothing
takes typing; the app keeps clear of it (and of the status and navigation bars). A hardware
keyboard and its shortcuts (Ctrl+S, Ctrl+B, Ctrl+Z, …) work as on the desktop.

## Files

- **Open** (File › Open, or Insert › Pictures) shows Android's file picker: device storage,
  an SD card, Google Drive and other installed storage apps.
- **Open with WordCraft** from a file manager, a mail attachment or another app's Share also works.
- **Save** (Ctrl+S or File › Save) of a document opened from a picker, or saved once already,
  writes it back to that file. A new document, **Save As** and every **Export** (PDF, PNG, …)
  ask where with Android's save picker, so a new document never lands on an older file that
  happens to share its name. The document takes the name you give it in the picker.
- **Print** uses Android's print dialog (Wi-Fi printers, Save as PDF, cloud print services).
- **Copy and Paste** share the system clipboard with other apps (text).
- Settings, added fonts, the custom dictionary and recovery files stay in the app's private
  storage. If Android closes the app in the background, unsaved work comes back when it reopens.
- File › Open doesn't list recent documents: Android gives the app a file's contents, not a path
  it could open again. The system picker behind **Browse…** has its own Recent list.

## Building

`build-apk.sh` builds the APK with the Android SDK's own tools (no Gradle):

```sh
rustup target add aarch64-linux-android
export ANDROID_HOME=~/Android/Sdk          # with build-tools, platforms;android-35 and an NDK
apps/wordcraft-android/build-apk.sh        # -> dist/android/WordCraft-<version>-arm64.apk
adb install -r dist/android/WordCraft-*-arm64.apk
adb logcat -s WordCraft                    # the app's log
```

The first build creates a signing key in `apps/wordcraft-android/keys/` (not committed). Keep
it: Android installs an update only over a build signed with the same key. `APP_ID` changes the
package name (default `wordcraft.android`).

The GitHub workflow `.github/workflows/android.yml` builds the APK on every push and attaches it
to the run; a tag `android-v*` publishes a release. Give it the secrets `ANDROID_KEYSTORE_BASE64`,
`ANDROID_KEYSTORE_PASSWORD` and `ANDROID_KEY_ALIAS` so every build carries the same key.

## How it fits together

| Piece | Job |
|---|---|
| `src/lib.rs` | `android_main`, the entry point NativeActivity calls |
| `src/shell.rs` | storage folders, `Services` (open, save, print, clipboard), preferences, recovery, the keyboard |
| `src/touch.rs` | finger gestures → clicks, scrolling and drag-select (tested on the desktop) |
| `src/keys.rs` | on-screen keyboard keys → egui keys and modifiers (tested on the desktop) |
| `src/mailbox.rs` | the inbox the Java side answers through, in order (tested on the desktop) |
| `src/bridge.rs` | JNI calls into the activity (the only `unsafe` in the port) |
| `android/java/…/WordCraftActivity.java` | `NativeActivity` subclass: pickers, saving, printing, clipboard, the keyboard's text field, window insets |
| `winit-android-modifiers.patch` | applied to winit for the Android build only: reports Shift/Ctrl/Alt, which winit 0.30 never does on Android |
| `android/AndroidManifest.xml` | the app, its launcher entry and the file types it opens |

The rest of WordCraft is shared with the desktop and web builds. Android changes outside this
folder: the font database scans Android's system font folders (`crates/fonts/src/fontdb.rs`),
recovery copies are locked with `flock` through rustix, since std's `File::try_lock` is
unsupported on Android (`crates/engine/src/recovery/desktop.rs`), and File › Open doesn't offer
WordCraft's own file browser, which on Android could only show the app's private folders
(`crates/ui-egui/src/backstage.rs`).

## Known limits

- arm64 only (no 32-bit ARM or x86 builds yet).
- The interface is the desktop one: it fits a tablet in landscape best; a phone is cramped.
- A mouse or trackpad works as a finger (no hover, no wheel).
- Pictures copied in another app don't paste yet (text does).
- The on-screen keyboard can't see the document's text, so its suggestions work word by word
  and it doesn't capitalize sentences itself (WordCraft's own AutoCorrect does).
- Two-finger gestures (pinch zoom, two-finger tap), double tap and portrait rotation are built
  and unit-tested but were not exercised on a device in the first test pass.

## Tested on

Samsung Galaxy Tab S9 (SM-X716B), Android 16, Vulkan on Adreno 740, Samsung Keyboard: start-up,
ribbon commands, typing with the on-screen keyboard (spaces, Backspace, Enter) and a hardware
keyboard (Ctrl+Z/Y/S, F12), scrolling and fling, long-press selection, copy and paste, Open,
Save, Save As, write-back on Save, cancelling a save, Export to PDF, the print dialog, Back,
leaving and returning to the app, and the layout around the status bar, navigation bar and
keyboard.

WordCraft is by the ArtCraft team, MIT OR Apache-2.0. This port is not made or supported by them.
