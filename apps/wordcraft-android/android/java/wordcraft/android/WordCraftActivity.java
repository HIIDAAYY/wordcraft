// WordCraft for Android: the activity that hosts the Rust app (libwordcraft_android.so).
//
// NativeActivity runs the app; this subclass adds what NDK code can't do alone. Rust calls the
// public methods below from its own thread (see src/bridge.rs); they start work on the UI thread
// and return at once. Answers go back as files in getFilesDir()/inbox, written under a .tmp name
// and renamed when complete (see src/mailbox.rs for the format).
//
// MIT OR Apache-2.0, like the rest of WordCraft.

package wordcraft.android;

import android.app.NativeActivity;
import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.Intent;
import android.content.SharedPreferences;
import android.content.UriPermission;
import android.database.Cursor;
import android.net.Uri;
import android.os.Bundle;
import android.os.CancellationSignal;
import android.os.ParcelFileDescriptor;
import android.print.PageRange;
import android.print.PrintAttributes;
import android.print.PrintDocumentAdapter;
import android.print.PrintDocumentInfo;
import android.print.PrintManager;
import android.os.Build;
import android.provider.OpenableColumns;
import android.text.InputType;
import android.util.Log;
import android.view.KeyCharacterMap;
import android.view.KeyEvent;
import android.view.View;
import android.view.ViewGroup;
import android.view.WindowInsets;
import android.view.WindowInsetsController;
import android.view.inputmethod.BaseInputConnection;
import android.view.inputmethod.EditorInfo;
import android.view.inputmethod.InputConnection;
import android.view.inputmethod.InputMethodManager;
import android.widget.Toast;

import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.List;
import java.util.Locale;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.atomic.AtomicLong;

public class WordCraftActivity extends NativeActivity {
    private static final String TAG = "WordCraft";
    private static final int REQ_OPEN = 41;
    private static final int REQ_SAVE = 42;
    /** Largest file read into the app (the Rust side caps it too). */
    private static final long MAX_BYTES = 512L << 20;
    /** Most files one multi-select pick (Add Font) delivers. */
    private static final int MAX_PICKED = 500;

    private final ExecutorService io = Executors.newSingleThreadExecutor();
    private final AtomicLong seq = new AtomicLong();
    private ClipboardManager clipboard;
    /** What the on-screen keyboard types into (see TextInput). */
    private TextInput textInput;
    /** The insets last reported, so an unchanged layout reports nothing. */
    private String lastInsets = "";
    /** Whether the app wants the on-screen keyboard (UI thread only). */
    private boolean keyboardWanted = false;

    // UI thread only: the save waiting for the save picker.
    private long pendingTicket = -1;
    private File pendingFile;
    private String pendingName;
    // UI thread only: what the open picker is for.
    private String openPurpose = "document";

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        // Before the app starts (super.onCreate starts it): the device's UTC offset and language,
        // for local dates and times, the paper size and the language new text is written in.
        writeSystemInfo();
        super.onCreate(savedInstanceState);
        new File(getFilesDir(), "inbox").mkdirs();
        deleteSharedPreferences("saved-locations");
        // The keyboard's text field: one invisible pixel in the corner.
        textInput = new TextInput(this);
        addContentView(textInput, new ViewGroup.LayoutParams(1, 1));
        // Android draws the app under the status bar, navigation bar and keyboard: tell the app
        // how much of each edge they cover, so it keeps its content clear of them.
        View decor = getWindow().getDecorView();
        decor.setOnApplyWindowInsetsListener((v, insets) -> {
            reportInsets(insets);
            return v.onApplyWindowInsets(insets);
        });
        darkSystemBarIcons();
        clipboard = (ClipboardManager) getSystemService(CLIPBOARD_SERVICE);
        if (clipboard != null) {
            clipboard.addPrimaryClipChangedListener(this::mirrorClipboard);
        }
        // A restart after the system ended the app keeps its intent: the file was already read.
        if (savedInstanceState == null) {
            handleIncoming(getIntent());
        }
    }

    @Override
    protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        setIntent(intent);
        handleIncoming(intent);
    }

    @Override
    public void onWindowFocusChanged(boolean hasFocus) {
        super.onWindowFocusChanged(hasFocus);
        // Android lets only the focused app read the clipboard.
        if (hasFocus) {
            mirrorClipboard();
            // Coming back from a picker that had the keyboard up: Android brings it back with
            // the window. Only the app decides whether it shows.
            if (!keyboardWanted) {
                setKeyboardVisible(false);
            }
        }
    }

    @Override
    protected void onDestroy() {
        io.shutdown();
        super.onDestroy();
    }

    /** Text the keyboard typed that has no key code (accents, emoji, a whole suggested word). */
    @Override
    public boolean dispatchKeyEvent(KeyEvent event) {
        if (event.getAction() == KeyEvent.ACTION_MULTIPLE && event.getKeyCode() == KeyEvent.KEYCODE_UNKNOWN) {
            String chars = event.getCharacters();
            if (chars != null && !chars.isEmpty()) {
                post("text", chars.getBytes(StandardCharsets.UTF_8));
            }
            return true;
        }
        return super.dispatchKeyEvent(event);
    }

    private void writeSystemInfo() {
        try {
            java.util.TimeZone tz = java.util.TimeZone.getDefault();
            long offset = tz.getOffset(System.currentTimeMillis()) / 1000L;
            String tag = Locale.getDefault().toLanguageTag();
            File file = new File(getFilesDir(), "system.txt");
            try (FileOutputStream out = new FileOutputStream(file)) {
                out.write((offset + "\n" + tag + "\n").getBytes(StandardCharsets.UTF_8));
            }
        } catch (Exception e) {
            Log.w(TAG, "can't record the time zone and language", e);
        }
    }

    // ---- Called from Rust ------------------------------------------------------------------

    /** The system picker for a document, picture, fonts or a recipient list. */
    public void openDocument(final String purpose) {
        runOnUiThread(() -> {
            openPurpose = purpose == null ? "document" : purpose;
            Intent intent = new Intent(Intent.ACTION_OPEN_DOCUMENT);
            intent.addCategory(Intent.CATEGORY_OPENABLE);
            // Many providers report Word files as application/octet-stream: documents, fonts and
            // lists show every file, and the app goes by the name.
            intent.setType("picture".equals(openPurpose) ? "image/*" : "*/*");
            if ("font".equals(openPurpose)) {
                intent.putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true);
            }
            intent.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION | Intent.FLAG_GRANT_WRITE_URI_PERMISSION
                    | Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION);
            try {
                startActivityForResult(intent, REQ_OPEN);
            } catch (Exception e) {
                Log.e(TAG, "no document picker", e);
                toast("No file picker is available on this device.");
                post("done", new byte[0]);
            }
        });
    }

    /** Save the staged file at path as name. With writeBack, over the file of that name the
     *  document was opened from or last saved to, when the app may still write there; otherwise
     *  (Save As, a first save, an export, or no such file) wherever the user picks. Reports the
     *  ticket's outcome with the name the file was saved under. */
    public void saveDocument(final long ticket, final String path, final String name, final String mime,
            final boolean writeBack) {
        runOnUiThread(() -> {
            final File staged = new File(path);
            final Uri known = writeBack ? knownUri(name) : null;
            if (known != null) {
                io.execute(() -> {
                    try {
                        copyFileToUri(staged, known);
                        staged.delete();
                        postSave(ticket, "ok", name);
                    } catch (Exception e) {
                        Log.w(TAG, "can't write back to " + known + ", asking where to save", e);
                        forget(name);
                        runOnUiThread(() -> startSavePicker(ticket, staged, name, mime));
                    }
                });
                return;
            }
            startSavePicker(ticket, staged, name, mime);
        });
    }

    /** Android's print dialog for a PDF. */
    public void printPdf(final String path, final String name) {
        runOnUiThread(() -> {
            PrintManager pm = (PrintManager) getSystemService(PRINT_SERVICE);
            if (pm == null) {
                toast("Printing is not available on this device.");
                return;
            }
            pm.print(name, new PdfAdapter(new File(path), name), new PrintAttributes.Builder().build());
        });
    }

    /** Put text on the system clipboard. */
    public void setClipboardText(final String text) {
        runOnUiThread(() -> {
            if (clipboard != null) {
                clipboard.setPrimaryClip(ClipData.newPlainText("WordCraft", text));
            }
        });
    }

    /** Show or hide the on-screen keyboard. */
    public void setKeyboardVisible(final boolean visible) {
        runOnUiThread(() -> {
            keyboardWanted = visible;
            InputMethodManager imm = (InputMethodManager) getSystemService(INPUT_METHOD_SERVICE);
            if (imm == null || textInput == null) {
                return;
            }
            if (visible) {
                textInput.requestFocus();
                // Bind the keyboard to the text field afresh: after another window (a picker) had
                // the keyboard, Android can show it again still bound to that window's connection,
                // and the keys then reach nothing.
                imm.restartInput(textInput);
                imm.showSoftInput(textInput, 0);
            } else {
                imm.hideSoftInputFromWindow(textInput.getWindowToken(), 0);
            }
        });
    }

    // ---- Answers ---------------------------------------------------------------------------

    @Override
    protected void onActivityResult(int requestCode, int resultCode, Intent data) {
        super.onActivityResult(requestCode, resultCode, data);
        if (requestCode == REQ_OPEN) {
            final List<Uri> uris = new ArrayList<>();
            if (resultCode == RESULT_OK && data != null) {
                if (data.getClipData() != null) {
                    for (int i = 0; i < data.getClipData().getItemCount() && i < MAX_PICKED; i++) {
                        uris.add(data.getClipData().getItemAt(i).getUri());
                    }
                } else if (data.getData() != null) {
                    uris.add(data.getData());
                }
            }
            final boolean document = "document".equals(openPurpose);
            for (Uri uri : uris) {
                if (document) {
                    keepPermission(uri, true);
                }
            }
            io.execute(() -> {
                for (Uri uri : uris) {
                    deliver(uri, document);
                }
                post("done", new byte[0]);
            });
        } else if (requestCode == REQ_SAVE) {
            final long ticket = pendingTicket;
            final File staged = pendingFile;
            pendingTicket = -1;
            pendingFile = null;
            pendingName = null;
            if (ticket < 0) {
                return;
            }
            if (resultCode != RESULT_OK || data == null || data.getData() == null) {
                if (staged != null) {
                    staged.delete();
                }
                postSave(ticket, "cancel", null);
                return;
            }
            final Uri uri = data.getData();
            keepPermission(uri, true);
            io.execute(() -> {
                try {
                    copyFileToUri(staged, uri);
                    // Remembered under the name the user gave it, which the document now takes;
                    // never under the suggested name, which a later new document may share.
                    String saved = displayName(uri);
                    remember(saved, uri);
                    postSave(ticket, "ok", saved);
                } catch (Exception e) {
                    Log.e(TAG, "save failed", e);
                    postSave(ticket, "fail", e.getMessage());
                } finally {
                    if (staged != null) {
                        staged.delete();
                    }
                }
            });
        }
    }

    private void startSavePicker(long ticket, File staged, String name, String mime) {
        if (pendingTicket >= 0) {
            staged.delete();
            postSave(ticket, "fail", "another save is still waiting for a location");
            return;
        }
        Intent intent = new Intent(Intent.ACTION_CREATE_DOCUMENT);
        intent.addCategory(Intent.CATEGORY_OPENABLE);
        intent.setType(mime == null || mime.isEmpty() ? "application/octet-stream" : mime);
        intent.putExtra(Intent.EXTRA_TITLE, name);
        intent.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION | Intent.FLAG_GRANT_WRITE_URI_PERMISSION
                | Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION);
        pendingTicket = ticket;
        pendingFile = staged;
        pendingName = name;
        try {
            startActivityForResult(intent, REQ_SAVE);
        } catch (Exception e) {
            Log.e(TAG, "no save picker", e);
            pendingTicket = -1;
            pendingFile = null;
            pendingName = null;
            staged.delete();
            postSave(ticket, "fail", "no save picker is available on this device");
        }
    }

    /** "Open with WordCraft" from a file manager, or a file shared to the app. */
    private void handleIncoming(Intent intent) {
        if (intent == null) {
            return;
        }
        Uri uri = null;
        if (Intent.ACTION_VIEW.equals(intent.getAction()) || Intent.ACTION_EDIT.equals(intent.getAction())) {
            uri = intent.getData();
        } else if (Intent.ACTION_SEND.equals(intent.getAction())) {
            uri = intent.getParcelableExtra(Intent.EXTRA_STREAM);
        }
        if (uri == null) {
            return;
        }
        final Uri incoming = uri;
        final boolean writable = (intent.getFlags() & Intent.FLAG_GRANT_WRITE_URI_PERMISSION) != 0;
        io.execute(() -> deliver(incoming, writable));
    }

    /** Stream one file into the inbox: its name, a newline, its bytes. */
    private void deliver(Uri uri, boolean rememberForSaving) {
        String name = displayName(uri);
        File dir = new File(getFilesDir(), "inbox");
        File tmp = new File(dir, String.format(Locale.ROOT, "%013d-%06d.open.tmp", System.currentTimeMillis(), seq.incrementAndGet()));
        try (InputStream in = getContentResolver().openInputStream(uri); OutputStream out = new FileOutputStream(tmp)) {
            if (in == null) {
                throw new IOException("the file can't be read");
            }
            out.write(name.replace('\n', ' ').getBytes(StandardCharsets.UTF_8));
            out.write('\n');
            byte[] buf = new byte[1 << 16];
            long total = 0;
            int n;
            while ((n = in.read(buf)) > 0) {
                total += n;
                if (total > MAX_BYTES) {
                    throw new IOException("larger than 512 MB");
                }
                out.write(buf, 0, n);
            }
        } catch (Exception e) {
            Log.e(TAG, "can't read " + uri, e);
            tmp.delete();
            toast("Couldn't open " + name + ": " + e.getMessage());
            return;
        }
        if (!tmp.renameTo(new File(dir, tmp.getName().substring(0, tmp.getName().length() - 4)))) {
            tmp.delete();
            return;
        }
        if (rememberForSaving) {
            remember(name, uri);
        }
    }

    private void copyFileToUri(File from, Uri to) throws IOException {
        if (from == null || !from.isFile()) {
            throw new IOException("nothing to save");
        }
        OutputStream out = null;
        for (String mode : new String[] {"wt", "rwt", "w"}) {
            try {
                out = getContentResolver().openOutputStream(to, mode);
                if (out != null) {
                    break;
                }
            } catch (Exception ignored) {
                // The provider doesn't take this mode; try the next.
            }
        }
        if (out == null) {
            throw new IOException("the location can't be written");
        }
        try (InputStream in = new FileInputStream(from); OutputStream o = out) {
            byte[] buf = new byte[1 << 16];
            int n;
            while ((n = in.read(buf)) > 0) {
                o.write(buf, 0, n);
            }
        }
    }

    private String displayName(Uri uri) {
        String name = null;
        try (Cursor c = getContentResolver().query(uri, new String[] {OpenableColumns.DISPLAY_NAME}, null, null, null)) {
            if (c != null && c.moveToFirst() && !c.isNull(0)) {
                name = c.getString(0);
            }
        } catch (Exception ignored) {
            // Some providers don't answer queries: fall back to the path.
        }
        if (name == null || name.isEmpty()) {
            name = uri.getLastPathSegment();
        }
        if (name == null || name.isEmpty()) {
            name = "document";
        }
        int slash = Math.max(name.lastIndexOf('/'), name.lastIndexOf('\\'));
        return slash >= 0 ? name.substring(slash + 1) : name;
    }

    // ---- Files the app may write back to ---------------------------------------------------

    private SharedPreferences known() {
        // v2: the first builds also filed a file under the name the app suggested, which a later
        // new document could share; those entries are dropped with the old store.
        return getSharedPreferences("saved-locations-v2", MODE_PRIVATE);
    }

    private void remember(String name, Uri uri) {
        if (name != null && uri != null) {
            known().edit().putString(name, uri.toString()).apply();
        }
    }

    private void forget(String name) {
        known().edit().remove(name).apply();
    }

    /** Where name was last opened from or saved to, while the app still has write permission. */
    private Uri knownUri(String name) {
        String s = known().getString(name, null);
        if (s == null) {
            return null;
        }
        Uri uri = Uri.parse(s);
        for (UriPermission p : getContentResolver().getPersistedUriPermissions()) {
            if (p.getUri().equals(uri) && p.isWritePermission()) {
                return uri;
            }
        }
        forget(name);
        return null;
    }

    private void keepPermission(Uri uri, boolean write) {
        try {
            int flags = Intent.FLAG_GRANT_READ_URI_PERMISSION | (write ? Intent.FLAG_GRANT_WRITE_URI_PERMISSION : 0);
            getContentResolver().takePersistableUriPermission(uri, flags);
        } catch (Exception e) {
            // Not every provider grants lasting access; saving asks where instead.
            Log.i(TAG, "no lasting access to " + uri + ": " + e.getMessage());
        }
    }

    // ---- Inbox -----------------------------------------------------------------------------

    private void post(String kind, byte[] data) {
        File dir = new File(getFilesDir(), "inbox");
        dir.mkdirs();
        String base = String.format(Locale.ROOT, "%013d-%06d.%s", System.currentTimeMillis(), seq.incrementAndGet(), kind);
        File tmp = new File(dir, base + ".tmp");
        try (FileOutputStream out = new FileOutputStream(tmp)) {
            out.write(data);
        } catch (IOException e) {
            Log.e(TAG, "inbox write failed", e);
            tmp.delete();
            return;
        }
        if (!tmp.renameTo(new File(dir, base))) {
            tmp.delete();
        }
    }

    private void postSave(long ticket, String outcome, String reason) {
        String text = ticket + "\n" + outcome + (reason != null ? "\n" + reason : "");
        post("save", text.getBytes(StandardCharsets.UTF_8));
    }

    private void mirrorClipboard() {
        String text = "";
        try {
            if (clipboard != null && clipboard.hasPrimaryClip()) {
                ClipData clip = clipboard.getPrimaryClip();
                if (clip != null && clip.getItemCount() > 0) {
                    CharSequence t = clip.getItemAt(0).coerceToText(this);
                    text = t == null ? "" : t.toString();
                }
            }
        } catch (Exception e) {
            Log.i(TAG, "clipboard unreadable: " + e.getMessage());
            return;
        }
        File file = new File(getFilesDir(), "clipboard.txt");
        File tmp = new File(getFilesDir(), "clipboard.txt.tmp");
        try (FileOutputStream out = new FileOutputStream(tmp)) {
            out.write(text.getBytes(StandardCharsets.UTF_8));
        } catch (IOException e) {
            tmp.delete();
            return;
        }
        if (!tmp.renameTo(file)) {
            tmp.delete();
        }
    }

    // ---- Keyboard text and keys -------------------------------------------------------------

    /** Text the keyboard typed. Enter and Tab inside it go as keys, so they act as keys do. */
    void postText(String text) {
        if (text == null || text.isEmpty()) {
            return;
        }
        StringBuilder run = new StringBuilder();
        for (int i = 0; i < text.length(); i++) {
            char c = text.charAt(i);
            if (c == '\n' || c == '\t') {
                if (run.length() > 0) {
                    post("text", run.toString().getBytes(StandardCharsets.UTF_8));
                    run.setLength(0);
                }
                postKey(c == '\n' ? KeyEvent.KEYCODE_ENTER : KeyEvent.KEYCODE_TAB, 0);
            } else {
                run.append(c);
            }
        }
        if (run.length() > 0) {
            post("text", run.toString().getBytes(StandardCharsets.UTF_8));
        }
    }

    void postKey(int keyCode, int metaState) {
        post("key", (keyCode + "\n" + metaState).getBytes(StandardCharsets.UTF_8));
    }

    /** A key event the keyboard sent: a printable key without Ctrl or Alt is text, the rest keys. */
    void postKeyEvent(KeyEvent event) {
        if (event.getAction() != KeyEvent.ACTION_DOWN) {
            if (event.getAction() == KeyEvent.ACTION_MULTIPLE && event.getCharacters() != null) {
                postText(event.getCharacters());
            }
            return;
        }
        int meta = event.getMetaState();
        boolean shortcut = (meta & (KeyEvent.META_CTRL_ON | KeyEvent.META_ALT_ON)) != 0;
        int code = event.getKeyCode();
        if (!shortcut && code != KeyEvent.KEYCODE_ENTER && code != KeyEvent.KEYCODE_TAB) {
            int ch = event.getUnicodeChar(meta);
            if (ch != 0 && (ch & KeyCharacterMap.COMBINING_ACCENT) == 0 && !Character.isISOControl(ch)) {
                postText(new String(Character.toChars(ch)));
                return;
            }
        }
        postKey(code, meta);
    }

    // ---- Window insets ---------------------------------------------------------------------

    private void reportInsets(WindowInsets insets) {
        int left, top, right, bottom;
        if (Build.VERSION.SDK_INT >= 30) {
            android.graphics.Insets i = insets.getInsets(WindowInsets.Type.systemBars()
                    | WindowInsets.Type.displayCutout() | WindowInsets.Type.ime());
            left = i.left;
            top = i.top;
            right = i.right;
            bottom = i.bottom;
        } else {
            left = insets.getSystemWindowInsetLeft();
            top = insets.getSystemWindowInsetTop();
            right = insets.getSystemWindowInsetRight();
            bottom = insets.getSystemWindowInsetBottom();
        }
        String text = left + " " + top + " " + right + " " + bottom;
        if (!text.equals(lastInsets)) {
            lastInsets = text;
            post("insets", text.getBytes(StandardCharsets.UTF_8));
        }
    }

    /** Until the app says otherwise: its default look is light, so dark bar icons. */
    private void darkSystemBarIcons() {
        applyBarIcons(true);
    }

    /** The app's background behind the status and navigation bars is light (dark icons) or dark
     *  (light icons). Called from Rust whenever the app's theme changes; the system's own dark
     *  mode doesn't decide this, the app's look does. */
    public void setLightSystemBars(final boolean light) {
        runOnUiThread(() -> applyBarIcons(light));
    }

    private void applyBarIcons(boolean light) {
        if (Build.VERSION.SDK_INT >= 30) {
            WindowInsetsController c = getWindow().getInsetsController();
            if (c != null) {
                int mask = WindowInsetsController.APPEARANCE_LIGHT_STATUS_BARS
                        | WindowInsetsController.APPEARANCE_LIGHT_NAVIGATION_BARS;
                c.setSystemBarsAppearance(light ? mask : 0, mask);
            }
        } else {
            View decor = getWindow().getDecorView();
            int flags = decor.getSystemUiVisibility();
            decor.setSystemUiVisibility(light ? flags | View.SYSTEM_UI_FLAG_LIGHT_STATUS_BAR
                    : flags & ~View.SYSTEM_UI_FLAG_LIGHT_STATUS_BAR);
        }
    }

    private void toast(final String message) {
        runOnUiThread(() -> Toast.makeText(this, message, Toast.LENGTH_LONG).show());
    }

    /** Hands a finished PDF to the print framework. */
    private static final class PdfAdapter extends PrintDocumentAdapter {
        private final File file;
        private final String name;

        PdfAdapter(File file, String name) {
            this.file = file;
            this.name = name;
        }

        @Override
        public void onLayout(PrintAttributes oldAttributes, PrintAttributes newAttributes, CancellationSignal cancel,
                LayoutResultCallback callback, Bundle extras) {
            if (cancel.isCanceled()) {
                callback.onLayoutCancelled();
                return;
            }
            PrintDocumentInfo info = new PrintDocumentInfo.Builder(name + ".pdf")
                    .setContentType(PrintDocumentInfo.CONTENT_TYPE_DOCUMENT)
                    .setPageCount(PrintDocumentInfo.PAGE_COUNT_UNKNOWN)
                    .build();
            callback.onLayoutFinished(info, true);
        }

        @Override
        public void onWrite(PageRange[] pages, ParcelFileDescriptor destination, CancellationSignal cancel,
                WriteResultCallback callback) {
            try (InputStream in = new FileInputStream(file); OutputStream out = new FileOutputStream(destination.getFileDescriptor())) {
                byte[] buf = new byte[1 << 16];
                int n;
                while ((n = in.read(buf)) > 0) {
                    if (cancel.isCanceled()) {
                        callback.onWriteCancelled();
                        return;
                    }
                    out.write(buf, 0, n);
                }
                callback.onWriteFinished(new PageRange[] {PageRange.ALL_PAGES});
            } catch (IOException e) {
                callback.onWriteFailed(e.getMessage());
            }
        }
    }

    /**
     * An invisible view the on-screen keyboard types into. NativeActivity's own view has no
     * InputConnection, so keyboards fall back to a mode that drops spaces and suggestions. This
     * one takes everything the keyboard sends and passes it to the app through the inbox, in
     * order: committed text as text, a word still being composed as the difference from the
     * last version (backspaces, then the new letters), and every other key as a key.
     */
    static final class TextInput extends View {
        private final WordCraftActivity activity;

        TextInput(WordCraftActivity activity) {
            super(activity);
            this.activity = activity;
            setFocusable(true);
            setFocusableInTouchMode(true);
        }

        @Override
        public boolean onCheckIsTextEditor() {
            return true;
        }

        @Override
        public InputConnection onCreateInputConnection(EditorInfo out) {
            // No automatic capitals from the keyboard: it can't see the document, and WordCraft's
            // own AutoCorrect capitalizes sentences as Word does.
            out.inputType = InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_FLAG_MULTI_LINE;
            out.imeOptions = EditorInfo.IME_FLAG_NO_FULLSCREEN | EditorInfo.IME_FLAG_NO_EXTRACT_UI
                    | EditorInfo.IME_ACTION_NONE;
            out.initialSelStart = 0;
            out.initialSelEnd = 0;
            return new Connection(this, activity);
        }
    }

    static final class Connection extends BaseInputConnection {
        private final WordCraftActivity activity;
        /** The word the keyboard is composing, as already typed into the document. */
        private String composing = "";

        Connection(View view, WordCraftActivity activity) {
            super(view, false);
            this.activity = activity;
        }

        /** Change what the document holds for the composing word from composing to next. */
        private void retype(String next) {
            int common = 0;
            int max = Math.min(composing.length(), next.length());
            while (common < max && composing.charAt(common) == next.charAt(common)) {
                common++;
            }
            // Don't split a surrogate pair.
            if (common > 0 && Character.isHighSurrogate(composing.charAt(common - 1))) {
                common--;
            }
            int remove = composing.codePointCount(common, composing.length());
            for (int i = 0; i < remove; i++) {
                activity.postKey(KeyEvent.KEYCODE_DEL, 0);
            }
            activity.postText(next.substring(common));
        }

        @Override
        public boolean commitText(CharSequence text, int newCursorPosition) {
            retype(text == null ? "" : text.toString());
            composing = "";
            return true;
        }

        @Override
        public boolean setComposingText(CharSequence text, int newCursorPosition) {
            String next = text == null ? "" : text.toString();
            retype(next);
            composing = next;
            return true;
        }

        @Override
        public boolean finishComposingText() {
            composing = "";
            return true;
        }

        @Override
        public boolean setComposingRegion(int start, int end) {
            // The app doesn't share its text with the keyboard, so there is nothing to recompose.
            return true;
        }

        @Override
        public boolean deleteSurroundingText(int beforeLength, int afterLength) {
            if (!composing.isEmpty()) {
                retype("");
                composing = "";
                beforeLength = 0;
            }
            for (int i = 0; i < beforeLength; i++) {
                activity.postKey(KeyEvent.KEYCODE_DEL, 0);
            }
            for (int i = 0; i < afterLength; i++) {
                activity.postKey(KeyEvent.KEYCODE_FORWARD_DEL, 0);
            }
            return true;
        }

        @Override
        public boolean deleteSurroundingTextInCodePoints(int beforeLength, int afterLength) {
            return deleteSurroundingText(beforeLength, afterLength);
        }

        @Override
        public boolean sendKeyEvent(KeyEvent event) {
            if (!composing.isEmpty() && event.getAction() == KeyEvent.ACTION_DOWN) {
                composing = "";
            }
            activity.postKeyEvent(event);
            return true;
        }

        @Override
        public boolean performEditorAction(int actionCode) {
            activity.postKey(KeyEvent.KEYCODE_ENTER, 0);
            return true;
        }

        @Override
        public CharSequence getTextBeforeCursor(int length, int flags) {
            // Keyboards look here to decide on capitals and what Backspace deletes: an empty
            // answer makes them send Backspace as a key, which the app handles.
            return "";
        }

        @Override
        public CharSequence getTextAfterCursor(int length, int flags) {
            return "";
        }

        @Override
        public int getCursorCapsMode(int reqModes) {
            return 0;
        }
    }
}
