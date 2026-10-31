//! The Android shell: storage folders, `Services` backed by the Java activity, the inbox,
//! finger gestures, the on-screen keyboard and the eframe runner.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use winit::platform::android::activity::AndroidApp;
use wordcraft_engine::Session;
use wordcraft_ui_egui::async_saves::{AsyncSaves, SaveOutcome};
use wordcraft_ui_egui::{Inbox, Services, UiState, WordApp};

use crate::mailbox::{self, Message, Outcome};
use crate::saving::Saving;
use crate::{bridge, keys, touch::Touches};

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
    // The device's language and time zone, which the activity wrote before starting the app.
    let system = SystemInfo::read(&files.join("system.txt"));
    // SAFETY: called on the app thread before eframe, the font database or any worker of ours
    // starts, so nothing in this process reads the environment concurrently. The settings folder
    // (`wordcraft_control_key::settings_dir`) and the user font folders read these variables; the
    // region (paper size, units, decimal comma) reads LANG, local dates and times the UTC offset.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("WORDCRAFT_PROFILE_DIR", &dirs.profile);
        std::env::set_var("HOME", &files);
        if let Some(lang) = &system.posix_locale {
            std::env::set_var("LANG", lang);
        }
        std::env::set_var("WORDCRAFT_UTC_OFFSET_SECS", system.utc_offset_secs.to_string());
    }
    log::info!("WordCraft {} for Android, data in {}", env!("CARGO_PKG_VERSION"), files.display());

    let (user_fonts_loaded, _) = wordcraft_fonts::FontDb::global().load_user_fonts(&dirs.profile.join("fonts"));
    if !user_fonts_loaded.is_empty() {
        log::info!("user fonts: {} loaded", user_fonts_loaded.len());
    }

    bridge::init(&android);
    let options = eframe::NativeOptions { android_app: Some(android), ..Default::default() };
    let result = eframe::run_native(
        "WordCraft",
        options,
        Box::new(move |cc| {
            if let Some(rs) = &cc.wgpu_render_state {
                log::info!("wgpu backend {:?} on {}", rs.adapter.get_info().backend, rs.adapter.get_info().name);
            }
            Ok(Box::new(Shell::new(&cc.egui_ctx, dirs, system.editing_language)))
        }),
    );
    if let Err(e) = result {
        log::error!("WordCraft stopped: {e}");
    }
}

/// What the activity reports of the device at start (`system.txt`: the UTC offset in seconds,
/// then the locale's BCP 47 tag).
struct SystemInfo {
    utc_offset_secs: i64,
    /// `id_ID.UTF-8` for `id-ID`.
    posix_locale: Option<String>,
    /// The tag new text is written in when the device's language isn't English (`id-ID`).
    editing_language: Option<String>,
}

impl SystemInfo {
    fn read(path: &Path) -> Self {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        let mut lines = text.lines();
        let utc_offset_secs = wordcraft_engine::cmd::parse_utc_offset(lines.next());
        let tag: String = lines.next().unwrap_or_default().trim().chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').take(35).collect();
        let mut parts = tag.split('-');
        let language = parts.next().unwrap_or_default().to_ascii_lowercase();
        let region = parts.find(|p| p.len() == 2).map(str::to_ascii_uppercase);
        let posix_locale = (language.len() >= 2).then(|| match &region {
            Some(r) => format!("{language}_{r}.UTF-8"),
            None => format!("{language}.UTF-8"),
        });
        let editing_language = (language.len() >= 2 && language != "en").then(|| match &region {
            Some(r) => format!("{language}-{r}"),
            None => language.clone(),
        });
        log::info!("device: UTC offset {utc_offset_secs} s, locale {tag:?}");
        Self { utc_offset_secs, posix_locale, editing_language }
    }
}

struct Shell {
    app: WordApp,
    dirs: Dirs,
    inbox: Inbox,
    saves: AsyncSaves,
    touches: Touches,
    /// Pickers and saves started and not yet answered (logged when the app closes with any).
    waiting: Rc<Cell<u32>>,
    /// The keyboard as last asked of the activity.
    keyboard: bool,
    /// Where the last tap or press began, and whether File view was open then: a tap shows the
    /// keyboard only when it lands where typing goes, and not when it opened a document.
    press: Option<(egui::Pos2, bool)>,
    /// Messages for the status bar from the host (a picker that couldn't open).
    messages: Rc<RefCell<Vec<String>>>,
    /// The preferences as last written.
    prefs_written: Option<String>,
    /// The window edges covered by system bars or the keyboard (pixels), as the activity last
    /// reported them.
    insets: Option<egui::epaint::MarginF32>,
    /// Saves handed to the activity ([`Saving`]).
    saving: Rc<RefCell<Saving>>,
    /// Whether the system bars' icons were last set for a light app (dark icons).
    light_bars: Option<bool>,
    /// Frames left in which the caret is brought into view: the keyboard just came up over it.
    reveal_caret: u8,
    /// The ribbon was collapsed to make room while the keyboard is up; it comes back with it.
    ribbon_auto_collapsed: bool,
    /// The language new text is written in, from the device's (`id-ID`), until applied.
    editing_language: Option<String>,
}

impl Shell {
    fn new(ctx: &egui::Context, dirs: Dirs, editing_language: Option<String>) -> Self {
        let inbox = Inbox::default();
        let saves = AsyncSaves::default();
        let waiting = Rc::new(Cell::new(0));
        let messages = Rc::new(RefCell::new(Vec::new()));
        let saving = Rc::new(RefCell::new(Saving::default()));
        let region = wordcraft_ui_egui::i18n::system_region();
        let mut session = Session::new(wordcraft_engine::blank_document(&region));
        session.system_region = region;
        let mut app = WordApp::new(session, services(&dirs, inbox.clone(), saves.clone(), waiting.clone(), messages.clone(), saving.clone()));
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
        watch_inbox(dirs.inbox.clone(), ctx.clone());
        Self {
            app,
            dirs,
            inbox,
            saves,
            touches: Touches::default(),
            waiting,
            keyboard: false,
            press: None,
            messages,
            prefs_written,
            insets: None,
            saving,
            light_bars: None,
            reveal_caret: 0,
            ribbon_auto_collapsed: false,
            editing_language,
        }
    }

    /// Deliver what the Java side left in the inbox.
    fn read_inbox(&mut self, raw: &mut egui::RawInput) {
        for message in mailbox::drain(&self.dirs.inbox) {
            match message {
                Message::Opened { name, bytes } => {
                    log::info!("received {name} ({} bytes)", bytes.len());
                    self.inbox.lock().unwrap_or_else(|e| e.into_inner()).push((name, bytes));
                    // The opened document takes typing straight away (a hardware keyboard), as on
                    // the desktop; the on-screen keyboard still waits for a tap.
                    self.app.canvas.want_focus = true;
                }
                Message::PickerClosed => self.waiting.set(self.waiting.get().saturating_sub(1)),
                Message::Saved { ticket, outcome, name } => {
                    self.waiting.set(self.waiting.get().saturating_sub(1));
                    let command = self.saving.borrow_mut().commands.remove(&ticket);
                    if let (Outcome::Saved, Some(command), Some(chosen)) = (&outcome, command, name) {
                        self.saved_as(&command, &chosen);
                    }
                    let outcome = match outcome {
                        Outcome::Saved => SaveOutcome::Saved,
                        Outcome::Cancelled => SaveOutcome::Cancelled,
                        Outcome::Failed(reason) => SaveOutcome::Failed(reason),
                    };
                    self.saves.finish(ticket, outcome);
                }
                Message::Text(text) => raw.events.push(egui::Event::Text(text)),
                Message::Key { code, meta } => raw.events.extend(keys::press(code, meta)),
                Message::Insets { left, top, right, bottom } => {
                    log::info!("window insets: left {left} top {top} right {right} bottom {bottom}");
                    // The keyboard came up: keep what's being typed in view above it, as Word does.
                    if self.insets.is_some_and(|old| bottom > old.bottom + 100.0) {
                        self.reveal_caret = 3;
                    }
                    self.insets = Some(egui::epaint::MarginF32 { left, right, top, bottom });
                }
            }
        }
    }

    /// The document was saved (Save or Save As) as the Word file `chosen`; the user may have
    /// renamed it in the save picker. The document takes that name, as after Save As on the
    /// desktop: the title says so and the next Save writes back to that file. Exports, and saves
    /// in formats that don't keep everything, leave the document's name alone.
    fn saved_as(&mut self, command: &str, chosen: &str) {
        let word = chosen.rsplit_once('.').is_some_and(|(_, ext)| wordcraft_engine::io::is_word_package(&ext.to_ascii_lowercase()));
        if matches!(command, "file.save" | "file.saveAs") && word {
            self.app.session.path = Some(PathBuf::from(chosen));
        }
    }

    /// Keep the save hooks' view of the document current, and the Recent list to files this
    /// device can open again. Android hands the app a file's contents, not a path to it, so a
    /// document opened or saved here is known only by its name; reopening one goes through
    /// File › Open, whose system picker has its own Recent list.
    fn sync(&mut self) {
        self.saving.borrow_mut().has_file = self.app.session.path.is_some();
        self.app.ui.recent.retain(|p| Path::new(p).is_absolute());
    }

    /// Write the preferences when they changed: Android may stop the app without warning.
    fn store_prefs(&mut self) {
        // The ribbon collapsed only for the keyboard is stored as it was.
        let collapsed = self.app.ui.ribbon_collapsed;
        if self.ribbon_auto_collapsed {
            self.app.ui.ribbon_collapsed = false;
        }
        let prefs = serde_json::to_string(&self.app.prefs());
        self.app.ui.ribbon_collapsed = collapsed;
        let Ok(json) = prefs else { return };
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
        self.sync();
        // Keep the app clear of the status bar, the navigation bar, a camera cutout and the
        // on-screen keyboard: Android draws apps edge to edge, under all of them.
        if let Some(px) = self.insets {
            let ppp = ctx.pixels_per_point().max(0.1);
            let points = egui::epaint::MarginF32 { left: px.left / ppp, right: px.right / ppp, top: px.top / ppp, bottom: px.bottom / ppp };
            raw.safe_area_insets = Some(egui::SafeAreaInsets(points));
        }
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
        self.app.raw_input_hook(raw);
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let messages = std::mem::take(&mut *self.messages.borrow_mut());
        for message in messages {
            self.app.status(message);
        }
        self.sync();
        self.app.logic(ctx);
        // New text in the device's language (Indonesian on an Indonesian tablet), as Word takes
        // the proofing language from the keyboard; WordCraft's English interface would write
        // English. Once: a language chosen in the app later stays.
        if let Some(tag) = self.editing_language.take() {
            self.app.session.set_editing_language(Some(&tag));
        }
        self.sync();
        self.store_prefs();
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Only the primary button (a finger's tap): a two-finger tap is a right-click, which opens
        // a menu, not the keyboard.
        let (pressed_at, released) = ui.ctx().input(|i| {
            let primary = i.pointer.button_pressed(egui::PointerButton::Primary);
            (primary.then(|| i.pointer.press_origin()).flatten(), i.pointer.button_released(egui::PointerButton::Primary))
        });
        if let Some(pos) = pressed_at {
            self.press = Some((pos, self.app.ui.backstage));
        }
        // Android draws the app under the status bar, the navigation bar and the keyboard: run it in
        // the part of the window they leave free (`content_rect`, from the insets), over the
        // app's own background.
        let free = ui.ctx().content_rect();
        ui.painter().rect_filled(ui.ctx().viewport_rect(), 0.0, ui.visuals().panel_fill);
        // The status and navigation bars' icons follow the app's theme (the system's dark mode
        // may differ from WordCraft's own theme setting).
        let light = !ui.visuals().dark_mode;
        if self.light_bars != Some(light) {
            self.light_bars = Some(light);
            if let Err(e) = bridge::light_system_bars(light) {
                log::warn!("system bars: {e}");
            }
        }
        if self.reveal_caret > 0 {
            self.reveal_caret -= 1;
            self.app.canvas.scroll_to_caret = true;
        }
        // With the keyboard up on a short screen (a tablet held sideways) the ribbon leaves only a
        // few lines of the page: it folds away while typing and comes back with the keyboard.
        let short = free.height() < 560.0;
        // The keyboard as the screen shows it (the user may put it away by hand).
        let keyboard_up = self.insets.is_some_and(|i| i.bottom > 200.0);
        if keyboard_up && short && !self.ribbon_auto_collapsed && !self.app.ui.ribbon_collapsed {
            self.app.ui.ribbon_collapsed = true;
            self.ribbon_auto_collapsed = true;
            self.reveal_caret = self.reveal_caret.max(2);
        } else if !keyboard_up && self.ribbon_auto_collapsed {
            self.app.ui.ribbon_collapsed = false;
            self.ribbon_auto_collapsed = false;
        }
        ui.scope_builder(egui::UiBuilder::new().max_rect(free), |ui| {
            ui.set_clip_rect(free);
            self.app.ui(ui);
        });
        let (typing_area, copied) = ui.ctx().output(|o| {
            let copied = o.commands.iter().rev().find_map(|c| match c {
                egui::OutputCommand::CopyText(t) if !t.is_empty() => Some(t.clone()),
                _ => None,
            });
            (o.ime.as_ref().map(|ime| ime.rect), copied)
        });
        // The shell decides when the keyboard shows, not egui-winit (which would show it whenever
        // the page has focus, even when a document has just been opened to read).
        ui.ctx().output_mut(|o| o.ime = None);
        if let Some(text) = copied
            && let Err(e) = bridge::set_clipboard(&text)
        {
            log::warn!("clipboard: {e}");
        }
        match typing_area {
            // Nothing takes typing: put the keyboard away.
            None if self.keyboard => {
                self.keyboard = false;
                if let Err(e) = bridge::keyboard(false) {
                    log::warn!("keyboard: {e}");
                }
            }
            // A tap where typing goes (anywhere on the page, or a text box), not one that opened a
            // document from File view: show the keyboard (again, if the user put it away).
            Some(area) if released => {
                let page = self.app.canvas.canvas_rect;
                if let Some((pos, from_file_view)) = self.press.take()
                    && !from_file_view
                    && (area.expand(8.0).contains(pos) || page.is_some_and(|r| r.contains(pos)))
                {
                    self.keyboard = true;
                    if let Err(e) = bridge::keyboard(true) {
                        log::warn!("keyboard: {e}");
                    }
                }
            }
            _ => {}
        }
        if released {
            self.press = None;
        }
    }

    fn on_exit(&mut self) {
        if self.waiting.get() > 0 {
            log::info!("closing with {} picker or save answers outstanding", self.waiting.get());
        }
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

/// Wake the app whenever the Java side leaves a message: its answers arrive while the app may be
/// idle, waiting for input. A glance at the folder every few milliseconds costs next to nothing.
fn watch_inbox(dir: PathBuf, ctx: egui::Context) {
    let spawned = std::thread::Builder::new().name("inbox".into()).spawn(move || {
        loop {
            if mailbox::has_mail(&dir) {
                ctx.request_repaint();
            }
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
    });
    if let Err(e) = spawned {
        log::warn!("inbox watcher: {e}");
    }
}

fn clear_dir(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        if let Err(err) = std::fs::remove_file(e.path()) {
            log::warn!("cannot remove {}: {err}", e.path().display());
        }
    }
}

fn services(
    dirs: &Dirs,
    inbox: Inbox,
    saves: AsyncSaves,
    waiting: Rc<Cell<u32>>,
    messages: Rc<RefCell<Vec<String>>>,
    saving: Rc<RefCell<Saving>>,
) -> Services {
    let command_saving = saving.clone();
    let open_waiting = waiting.clone();
    let outbox = dirs.outbox.clone();
    let print_outbox = dirs.outbox.clone();
    let clipboard = dirs.clipboard.clone();
    let save_saves = saves.clone();
    Services {
        open_async: Some(Box::new(move |purpose: &str| match bridge::open(purpose) {
            Ok(()) => open_waiting.set(open_waiting.get().saturating_add(1)),
            Err(e) => {
                log::error!("cannot show the file picker: {e}");
                messages.borrow_mut().push(format!("Couldn't show the file picker: {e}"));
            }
        })),
        // Exports ask for a name in the app; Android's picker then asks where.
        pick_save: Some(Box::new(|name: &str| Some(name.to_string()))),
        download_command: Some(Box::new(move |command: &str| command_saving.borrow_mut().command = command.to_string())),
        async_saves: Some(saves),
        download: Some(Box::new(move |path: &str, bytes: &[u8]| save(&outbox, &save_saves, &waiting, &saving, path, bytes))),
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

/// Hand `bytes` to the activity to save as `path`'s file name: back to the file of that name the
/// document was opened from or last saved to, or else where the user picks. The save finishes
/// later: its ticket ends when the activity reports back through the inbox.
fn save(outbox: &Path, saves: &AsyncSaves, waiting: &Rc<Cell<u32>>, saving: &Rc<RefCell<Saving>>, path: &str, bytes: &[u8]) -> Result<(), String> {
    let name = mailbox::safe_name(path);
    let (write_back, command) = {
        let mut s = saving.borrow_mut();
        let write_back = s.writes_back();
        (write_back, std::mem::take(&mut s.command))
    };
    let ticket = saves.start();
    let file = outbox.join(format!("{ticket}-{name}"));
    let started = std::fs::write(&file, bytes)
        .map_err(|e| format!("cannot stage the file: {e}"))
        .and_then(|()| bridge::save(ticket, &file.to_string_lossy(), &name, mailbox::mime_for(&name), write_back));
    match started {
        Ok(()) => {
            saving.borrow_mut().commands.insert(ticket, command);
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
