/**
 * The clipboard helper (spec section 11).
 *
 * jsdom implements neither `navigator.clipboard` nor
 * `document.execCommand("copy")`, so both halves are installed explicitly here
 * and the test says out loud which one a case is exercising. That is the point
 * of the file: the async API is not uniformly available in a Tauri webview, and
 * a copy that silently does nothing is the defect being fixed.
 */

import { afterEach, describe, expect, it } from "vitest";
import { copyText, readClipboardText } from "../services/clipboard";
import {
  removeClipboardApi,
  stubClipboardApi,
  stubExecCommand,
} from "./mock-xterm";

afterEach(() => {
  // Both stubs are own properties on live globals; drop them so one test cannot
  // decide the environment of the next.
  Reflect.deleteProperty(navigator, "clipboard");
  Reflect.deleteProperty(document, "execCommand");
});

describe("copyText: the async Clipboard API", () => {
  it("uses navigator.clipboard.writeText and reports success", async () => {
    const clipboard = stubClipboardApi();
    const execCommand = stubExecCommand();

    await expect(copyText("hello")).resolves.toBe(true);

    expect(clipboard.writeText).toHaveBeenCalledWith("hello");
    // The legacy path is not touched when the real API works.
    expect(execCommand).not.toHaveBeenCalled();
  });

  it("falls back to the textarea + execCommand path when the API rejects", async () => {
    // The realistic failure: the document is not focused, so the promise
    // rejects even though the API exists.
    const clipboard = stubClipboardApi(async () => {
      throw new Error("Document is not focused");
    });
    // What the fallback saw while it was copying: it must have a real,
    // selected textarea in the document, which is why this is captured from
    // inside the (stubbed) execCommand call rather than read afterwards.
    const documentScratch: { textarea?: HTMLTextAreaElement | null } = {};
    const execCommand = stubExecCommand();
    execCommand.mockImplementation(() => {
      documentScratch.textarea = document.querySelector("textarea");
      return true;
    });

    await expect(copyText("selected text")).resolves.toBe(true);

    expect(clipboard.writeText).toHaveBeenCalledWith("selected text");
    expect(execCommand).toHaveBeenCalledWith("copy");
    // The fallback really selected the text: an element that cannot hold a
    // selection (display:none) would make the copy a no-op.
    const captured = documentScratch.textarea;
    expect(captured).toBeDefined();
    expect(captured?.value).toBe("selected text");
    // ...and it is removed again, so nothing is left in the document.
    expect(document.querySelector("textarea")).toBeNull();
  });
});

describe("copyText: no async API at all", () => {
  it("uses the execCommand fallback when navigator.clipboard is absent", async () => {
    removeClipboardApi();
    const execCommand = stubExecCommand();

    await expect(copyText("fallback")).resolves.toBe(true);

    expect(execCommand).toHaveBeenCalledWith("copy");
    expect(document.querySelector("textarea")).toBeNull();
  });

  it("reports failure - rather than pretending - when both paths are unavailable", async () => {
    removeClipboardApi();
    // jsdom's real state: no execCommand implementation either.
    Reflect.deleteProperty(document, "execCommand");

    await expect(copyText("nowhere to go")).resolves.toBe(false);
  });

  it("reports failure when execCommand refuses the copy", async () => {
    removeClipboardApi();
    const execCommand = stubExecCommand(false);

    await expect(copyText("refused")).resolves.toBe(false);

    expect(execCommand).toHaveBeenCalledWith("copy");
    expect(document.querySelector("textarea")).toBeNull();
  });

  it("does not touch the clipboard for empty text", async () => {
    const clipboard = stubClipboardApi();
    const execCommand = stubExecCommand();

    await expect(copyText("")).resolves.toBe(false);

    expect(clipboard.writeText).not.toHaveBeenCalled();
    expect(execCommand).not.toHaveBeenCalled();
  });
});

describe("readClipboardText", () => {
  it("returns what the clipboard holds", async () => {
    const clipboard = stubClipboardApi(async () => undefined, async () => "ls -la");

    await expect(readClipboardText()).resolves.toBe("ls -la");
    expect(clipboard.readText).toHaveBeenCalledTimes(1);
  });

  it("answers null when the API is absent (the caller must not destroy anything)", async () => {
    removeClipboardApi();

    await expect(readClipboardText()).resolves.toBeNull();
  });

  it("answers null when the read is refused", async () => {
    stubClipboardApi(async () => undefined, async () => {
      throw new Error("NotAllowedError");
    });

    await expect(readClipboardText()).resolves.toBeNull();
  });
});
