//! The Android shell: storage folders, `Services` backed by the Java activity, the inbox,
//! finger gestures, the on-screen keyboard and the eframe runner.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use winit::platform::android::activity::AndroidApp;
use wordcraft_engine::Session;
use wordcraft_ui_egui::async_saves::{AsyncSaves, SaveOutcome};
use wordcraft_ui_egui::{Inbox, Services, UiState, WordApp};

use crate::mailbox::{self, Message, Outcome};
use crate::{bridge, touch::Touches};

/// The app's private folders (under `Context.getFilesDir()`, the same folders the Java side uses).
#[derive(Clone)]
struct Dirs {
    /// Settings, added fonts, the user dictionary, templates and recovery files.
    profile: PathBuf,
    /// Messages from Java ([`mailbox`]).
    inbox: PathBuf,
    /// Files waiting for Java to save or print them.
    outbox: PathBuf,
    /// The system clipboard's text, as the activity last saw it.
    clipboard: PathBuf,
}

impl Dirs {
    fn new(files: &Path) -> Self {
        Self { profile: files.join("profile"), inbox: files.join("inbox"), outbox: files.join("outbox"), clipboard: files.join("clipboard.txt") }
    }
}

pub fn run(android: AndroidApp) {
    android_logger::init_once(android_logger::Config::default().with_max_level(log::LevelFilter::Info).with_tag("WordCraft"));
    std::panic::set_hook(Box::new(|info| log::error!("panic: {info}")));
    let files = android.internal_data_path().unwrap_or_else(|| PathBuf::from("/data/local/tmp/wordcraft"));
    let dirs = Dirs::new(&files);
    for d in [&dirs.profile, &dirs.inbox, &dirs.outbox] {
        if let Err(e) = std::fs::create_dir_all(d) {
            log::warn!("cannot create {}: {e}", d.display());
        }
    }
    // Files left in the outbox were handed to an earlier run's save picker and are done with.
    clear_dir(&dirs.outbox);
    // SAFETY: called on the app thread before eframe, the font database or any worker of ours
    // starts, so nothing in this process reads the environment concurrently. The settings folder
    // (`wordcraft_control_key::settings_dir`) and the user font folders read these variables.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("WORDCRAFT_PROFILE_DIR", &dirs.profile);
        std::env::set_var("HOME", &files);
    }
    log::info!("WordCraft {} for Android, data in {}", env!("CARGO_PKG_VERSION"), files.display());

    let (user_fonts_loaded, _) = wordcraft_fonts::FontDb::global().load_user_fonts(&dirs.profile.join("fonts"));
    if !user_fonts_loaded.is_empty() {
        log::info!("user fonts: {} loaded", user_fonts_loaded.len());
    }

    let options = eframe::NativeOptions { android_app: Some(android), ..Default::default() };
    let result = eframe::run_native(
        "WordCraft",
        options,
        Box::new(move |cc| {
            if let Some(rs) = &cc.wgpu_render_state {
                log::info!("wgpu backend {:?} on {}", rs.adapter.get_info().backend, rs.adapter.get_info().name);
            }
            Ok(Box::new(Shell::new(&cc.egui_ctx, dirs)))
        }),
    );
    if let Err(e) = result {
        log::error!("WordCraft stopped: {e}");
    }
}

struct Shell {
    app: WordApp,
    dirs: Dirs,
    inbox: Inbox,
    saves: AsyncSaves,
    touches: Touches,
    /// Pickers and saves started and not yet answered: while any are, the app looks at the inbox
    /// a few times a second (an answer doesn't wake it).
    waiting: Rc<Cell<u32>>,
    /// The keyboard as last asked of the activity.
    keyboard: bool,
    /// The preferences as last written.
    prefs_written: Option<String>,
}

impl Shell {
    fn new(ctx: &egui::Context, dirs: Dirs) -> Self {
        let inbox = Inbox::default();
        let saves = AsyncSaves::default();
        let waiting = Rc::new(Cell::new(0));
        let region = wordcraft_ui_egui::i18n::system_region();
        let mut session = Session::new(wordcraft_engine::blank_document(&region));
        session.system_region = region;
        let mut app = WordApp::new(session, services(&dirs, inbox.clone(), saves.clone(), waiting.clone()));
        let prefs_path = dirs.profile.join("ui.json");
        let stored = std::fs::read(&prefs_path).ok();
        if let Some(ui) = stored.as_deref().and_then(UiState::from_json) {
            app.apply_prefs(ui);
        }
        let dict = dirs.profile.join(Session::USER_DICT_FILE);
        wordcraft_proof::load_user_dictionary(&dict);
        app.session.user_dict_path = Some(dict);
        if app.session.prefs.region.is_some() {
            let doc = app.session.blank_document();
            app.session.set_document(doc);
        }
        // Android may end the app whenever it is in the background: unsaved work comes back
        // from the recovery folder on the next start.
        let repaint = ctx.clone();
        if let Err(e) = app.session.enable_recovery(dirs.profile.join("recovery"), std::sync::Arc::new(move || repaint.request_repaint())) {
            log::warn!("local recovery unavailable: {e}");
        }
        // No document yet: File › Home, as on the desktop. A file opened from another app arrives
        // through the inbox on the first frame and replaces it.
        app.start_screen();
        let prefs_written = serde_json::to_string(&app.prefs()).ok();
        Self { app, dirs, inbox, saves, touches: Touches::default(), waiting, keyboard: false, prefs_written }
    }

    /// Deliver what the Java side left in the inbox.
    fn read_inbox(&mut self, raw: &mut egui::RawInput) {
        for message in mailbox::drain(&self.dirs.inbox) {
            match message {
                Message::Opened { name, bytes } => {
                    log::info!("received {name} ({} bytes)", bytes.len());
                    self.inbox.lock().unwrap_or_else(|e| e.into_inner()).push((name, bytes));
                }
                Message::PickerClosed => self.waiting.set(self.waiting.get().saturating_sub(1)),
                Message::Saved { ticket, outcome } => {
                    self.waiting.set(self.waiting.get().saturating_sub(1));
                    let outcome = match outcome {
                        Outcome::Saved => SaveOutcome::Saved,
                        Outcome::Cancelled => SaveOutcome::Cancelled,
                        Outcome::Failed(reason) => SaveOutcome::Failed(reason),
                    };
                    self.saves.finish(ticket, outcome);
                }
                Message::Text(text) => raw.events.push(egui::Event::Text(text)),
            }
        }
    }

    /// Write the preferences when they changed: Android may stop the app without warning.
    fn store_prefs(&mut self) {
        let Ok(json) = serde_json::to_string(&self.app.prefs()) else { return };
        if self.prefs_written.as_deref() == Some(json.as_str()) {
            return;
        }
        let path = self.dirs.profile.join("ui.json");
        let tmp = self.dirs.profile.join(".ui.json.tmp");
        match std::fs::write(&tmp, &json).and_then(|()| std::fs::rename(&tmp, &path)) {
            Ok(()) => self.prefs_written = Some(json),
            Err(e) => log::warn!("preferences: cannot save {}: {e}", path.display()),
        }
    }
}

impl eframe::App for Shell {
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        self.read_inbox(raw);
        for event in &mut raw.events {
            match event {
                // The system Back button closes what's open, as Escape does.
                egui::Event::Key { key: key @ egui::Key::BrowserBack, .. } => *key = egui::Key::Escape,
                // Paste takes the system clipboard (another app's copy), when it has text.
                egui::Event::Paste(text) => {
                    if let Some(system) = clipboard_text(&self.dirs.clipboard) {
                        *text = system;
                    }
                }
                _ => {}
            }
        }
        if self.touches.process(raw) {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
        if self.waiting.get() > 0 {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
        self.app.raw_input_hook(raw);
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.app.logic(ctx);
        self.store_prefs();
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.app.ui(ui);
        let (wants_keyboard, copied) = ui.ctx().output(|o| {
            let copied = o.commands.iter().rev().find_map(|c| match c {
                egui::OutputCommand::CopyText(t) if !t.is_empty() => Some(t.clone()),
                _ => None,
            });
            (o.ime.is_some(), copied)
        });
        if let Some(text) = copied
            && let Err(e) = bridge::set_clipboard(&text)
        {
            log::warn!("clipboard: {e}");
        }
        if wants_keyboard != self.keyboard {
            self.keyboard = wants_keyboard;
            if let Err(e) = bridge::keyboard(wants_keyboard) {
                log::warn!("keyboard: {e}");
            }
        }
    }

    fn on_exit(&mut self) {
        self.store_prefs();
        self.app.session.finish_recovery();
    }
}

/// The text on the system clipboard, as the activity last saw it (it reads it whenever the app
/// comes to the front or the clipboard changes; Android lets only the focused app read it).
fn clipboard_text(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() > 16 << 20 {
        return None;
    }
    std::fs::read_to_string(path).ok().filter(|t| !t.is_empty())
}

fn clear_dir(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        if let Err(err) = std::fs::remove_file(e.path()) {
            log::warn!("cannot remove {}: {err}", e.path().display());
        }
    }
}

fn services(dirs: &Dirs, inbox: Inbox, saves: AsyncSaves, waiting: Rc<Cell<u32>>) -> Services {
    let open_waiting = waiting.clone();
    let outbox = dirs.outbox.clone();
    let print_outbox = dirs.outbox.clone();
    let clipboard = dirs.clipboard.clone();
    let save_saves = saves.clone();
    Services {
        open_async: Some(Box::new(move |purpose: &str| match bridge::open(purpose) {
            Ok(()) => open_waiting.set(open_waiting.get().saturating_add(1)),
            Err(e) => log::error!("cannot show the file picker: {e}"),
        })),
        // Save and Export ask for a name in the app; Android's picker then asks where.
        pick_save: Some(Box::new(|name: &str| Some(name.to_string()))),
        async_saves: Some(saves),
        download: Some(Box::new(move |path: &str, bytes: &[u8]| save(&outbox, &save_saves, &waiting, path, bytes))),
        print: Some(Box::new(move |bytes: &[u8]| {
            let file = print_outbox.join("print.pdf");
            std::fs::write(&file, bytes).map_err(|e| e.to_string())?;
            bridge::print(&file.to_string_lossy(), "WordCraft document")
        })),
        inbox: Some(inbox),
        clipboard_text: Some(Box::new(move || clipboard_text(&clipboard))),
        templates_dir: Some(dirs.profile.join("Templates")),
        ..Default::default()
    }
}

/// Hand `bytes` to Android's save picker as `path`'s file name. The save finishes later: its
/// ticket ends when the activity reports back through the inbox.
fn save(outbox: &Path, saves: &AsyncSaves, waiting: &Rc<Cell<u32>>, path: &str, bytes: &[u8]) -> Result<(), String> {
    let name = mailbox::safe_name(path);
    let ticket = saves.start();
    let file = outbox.join(format!("{ticket}-{name}"));
    let started = std::fs::write(&file, bytes)
        .map_err(|e| format!("cannot stage the file: {e}"))
        .and_then(|()| bridge::save(ticket, &file.to_string_lossy(), &name, mailbox::mime_for(&name)));
    match started {
        Ok(()) => {
            waiting.set(waiting.get().saturating_add(1));
            Ok(())
        }
        Err(e) => {
            let _ = std::fs::remove_file(&file);
            saves.finish(ticket, SaveOutcome::Failed(e.clone()));
            Err(e)
        }
    }
}
