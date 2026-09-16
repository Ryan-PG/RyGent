/**
 * Clipboard access for the terminal's copy/paste path (spec section 11).
 *
 * Not a Tauri bridge, unlike every other module in `services/`: the clipboard is
 * a browser platform API here, and the terminal both produces (copy) and
 * consumes (paste) text with it.
 *
 * Why this is not just `navigator.clipboard.writeText`: the terminal runs inside
 * the Tauri webview (WebView2 on Windows, WKWebView on macOS, webkit2gtk on
 * Linux), where the async clipboard API is not uniformly usable - it is absent
 * outside a secure context, and when it exists it can still reject because the
 * document is not focused (exactly what happens when the user clicks a toolbar
 * button, or returns to the window). A copy that silently does nothing is the
 * defect this module exists to fix, so:
 *
 * - writing tries `navigator.clipboard.writeText` first and falls back to the
 *   deprecated-but-universally-present `document.execCommand("copy")` on a
 *   hidden textarea;
 * - reading has no fallback (there is no synchronous read that does not first
 *   destroy the user's clipboard), so it reports failure and the caller leaves
 *   the native Ctrl+V path alone.
 *
 * Both functions report the outcome instead of pretending: a caller that wants
 * to tell the user "that copy did not happen" gets a `false`.
 */

/**
 * Copy `text` to the clipboard.
 *
 * @returns `true` only when the text really reached the clipboard.
 */
export async function copyText(text: string): Promise<boolean> {
  if (text.length === 0) {
    return false;
  }
  if (await writeWithClipboardApi(text)) {
    return true;
  }
  return writeWithExecCommand(text);
}

/**
 * Read the clipboard, or `null` when the platform will not give it to us.
 *
 * `null` is the ordinary answer in a webview that has no async clipboard API or
 * has not been granted permission - the caller must treat it as "nothing to
 * paste", never as an empty clipboard.
 */
export async function readClipboardText(): Promise<string | null> {
  const clipboard =
    typeof navigator === "undefined" ? undefined : navigator.clipboard;
  if (clipboard === undefined || typeof clipboard.readText !== "function") {
    return null;
  }
  try {
    return await clipboard.readText();
  } catch {
    // Permission denied, no focus, no clipboard at all: all the same to us.
    return null;
  }
}

/** Async Clipboard API half of {@link copyText}. */
async function writeWithClipboardApi(text: string): Promise<boolean> {
  const clipboard =
    typeof navigator === "undefined" ? undefined : navigator.clipboard;
  if (clipboard === undefined || typeof clipboard.writeText !== "function") {
    return false;
  }
  try {
    await clipboard.writeText(text);
    return true;
  } catch {
    // Refused (unfocused document, denied permission): try the legacy path.
    return false;
  }
}

/**
 * Legacy copy: a selected, invisible textarea plus `document.execCommand`.
 *
 * The textarea is positioned off-screen rather than hidden with `display: none`
 * or `visibility: hidden`, because neither can hold a selection - a hidden
 * element cannot be selected, so the copy would fail. `readonly` keeps mobile
 * keyboards and IMEs out of it.
 */
function writeWithExecCommand(text: string): boolean {
  if (
    typeof document === "undefined" ||
    typeof document.execCommand !== "function"
  ) {
    return false;
  }
  const textarea = document.createElement("textarea");
  textarea.value = text;
  textarea.setAttribute("readonly", "");
  textarea.setAttribute("aria-hidden", "true");
  textarea.style.position = "fixed";
  textarea.style.top = "-1000px";
  textarea.style.left = "0";
  textarea.style.opacity = "0";
  document.body.appendChild(textarea);

  // The shell that opened the copy (a menu item, the toolbar) is what had focus;
  // without restoring it the terminal would stop accepting keystrokes.
  const previousFocus =
    document.activeElement instanceof HTMLElement ? document.activeElement : null;
  let copied = false;
  try {
    textarea.select();
    textarea.setSelectionRange(0, textarea.value.length);
    copied = document.execCommand("copy");
  } catch {
    copied = false;
  } finally {
    textarea.remove();
    previousFocus?.focus();
  }
  return copied;
}
