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
import android.provider.OpenableColumns;
import android.util.Log;
import android.view.KeyEvent;
import android.view.View;
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

    // UI thread only: the save waiting for the save picker.
    private long pendingTicket = -1;
    private File pendingFile;
    private String pendingName;
    // UI thread only: what the open picker is for.
    private String openPurpose = "document";

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        View decor = getWindow().getDecorView();
        decor.setFocusable(true);
        decor.setFocusableInTouchMode(true);
        new File(getFilesDir(), "inbox").mkdirs();
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

    /** Save the staged file at path as name: back to the file it was opened from when the app may
     *  still write there, otherwise wherever the user picks. Reports ticket's outcome. */
    public void saveDocument(final long ticket, final String path, final String name, final String mime) {
        runOnUiThread(() -> {
            final File staged = new File(path);
            final Uri known = knownUri(name);
            if (known != null) {
                io.execute(() -> {
                    try {
                        copyFileToUri(staged, known);
                        staged.delete();
                        postSave(ticket, "ok", null);
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
            View decor = getWindow().getDecorView();
            InputMethodManager imm = (InputMethodManager) getSystemService(INPUT_METHOD_SERVICE);
            if (imm == null) {
                return;
            }
            if (visible) {
                decor.requestFocus();
                imm.showSoftInput(decor, InputMethodManager.SHOW_FORCED);
            } else {
                imm.hideSoftInputFromWindow(decor.getWindowToken(), 0);
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
            final String name = pendingName;
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
                    remember(name, uri);
                    postSave(ticket, "ok", null);
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
        return getSharedPreferences("saved-locations", MODE_PRIVATE);
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
}
