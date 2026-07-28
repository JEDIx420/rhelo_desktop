import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import {
  isSelectionSnapshotValid,
  getSessionDeleteConfirmationMessage,
  normalizeSessionHtml,
  resolveFontSizeState,
  runConfirmedSessionDeletion,
  SessionSaveCoordinator,
  shouldLoadSessionContent,
  type SessionSaveSnapshot,
} from "../src/lib/sessionEditor";
import { flushActiveSessionEdits, registerSessionSaveFlusher } from "../src/lib/sessionSaveBridge";

const wait = (milliseconds: number) => new Promise((resolve) => setTimeout(resolve, milliseconds));

test("scheduled saves retain the session ID and newest snapshot", async () => {
  const persisted: SessionSaveSnapshot[] = [];
  const coordinator = new SessionSaveCoordinator({
    delayMs: 5,
    persist: async (snapshot) => {
      persisted.push(snapshot);
    },
  });

  coordinator.schedule({ sessionId: "A", title: "A", content: "<p>old</p>" });
  coordinator.schedule({ sessionId: "A", title: "A", content: "<p>new</p>" });
  coordinator.schedule({ sessionId: "B", title: "B", content: "<p>other</p>" });
  await coordinator.flushAll();

  assert.deepEqual(
    persisted.map(({ sessionId, content }) => ({ sessionId, content })),
    [
      { sessionId: "A", content: "<p>new</p>" },
      { sessionId: "B", content: "<p>other</p>" },
    ],
  );
});

test("a session switch cannot redirect an in-flight save", async () => {
  const persisted: Array<{ sessionId: string; content: string }> = [];
  let releaseFirstSave: (() => void) | undefined;
  const firstSaveBlocked = new Promise<void>((resolve) => {
    releaseFirstSave = resolve;
  });
  const coordinator = new SessionSaveCoordinator({
    persist: async (snapshot) => {
      if (snapshot.content === "A1") await firstSaveBlocked;
      persisted.push({ sessionId: snapshot.sessionId, content: snapshot.content });
    },
  });

  coordinator.schedule({ sessionId: "A", title: "A", content: "A1" });
  const firstFlush = coordinator.flush("A");
  await wait(0);
  coordinator.schedule({ sessionId: "B", title: "B", content: "B1" });
  releaseFirstSave?.();
  await firstFlush;
  await coordinator.flush("B");

  assert.deepEqual(persisted, [
    { sessionId: "A", content: "A1" },
    { sessionId: "B", content: "B1" },
  ]);
});

test("newer saves are serialized after older in-flight saves", async () => {
  const persisted: string[] = [];
  let releaseFirstSave: (() => void) | undefined;
  const firstSaveBlocked = new Promise<void>((resolve) => {
    releaseFirstSave = resolve;
  });
  const coordinator = new SessionSaveCoordinator({
    persist: async (snapshot) => {
      if (snapshot.content === "first") await firstSaveBlocked;
      persisted.push(snapshot.content);
    },
  });

  coordinator.schedule({ sessionId: "A", title: "A", content: "first" });
  const firstFlush = coordinator.flush("A");
  await wait(0);
  coordinator.schedule({ sessionId: "A", title: "A", content: "second" });
  const secondFlush = coordinator.flush("A");
  releaseFirstSave?.();
  await Promise.all([firstFlush, secondFlush]);

  assert.deepEqual(persisted, ["first", "second"]);
});

test("deleted sessions discard stale delayed writes", async () => {
  const persisted: SessionSaveSnapshot[] = [];
  const coordinator = new SessionSaveCoordinator({
    delayMs: 5,
    persist: async (snapshot) => {
      persisted.push(snapshot);
    },
  });

  coordinator.schedule({ sessionId: "deleted", title: "Deleted", content: "stale" });
  coordinator.discardSession("deleted");
  await wait(10);
  await coordinator.flushAll();

  assert.deepEqual(persisted, []);
  assert.throws(
    () => coordinator.schedule({ sessionId: "deleted", title: "Deleted", content: "later" }),
    /inactive study session/,
  );
});

test("flushAll persists pending unmount or navigation work", async () => {
  const persisted: string[] = [];
  const coordinator = new SessionSaveCoordinator({
    delayMs: 60_000,
    persist: async (snapshot) => {
      persisted.push(snapshot.content);
    },
  });

  coordinator.schedule({ sessionId: "A", title: "A", content: "latest editor state" });
  assert.equal(coordinator.hasPending(), true);
  await coordinator.flushAll();
  coordinator.dispose();

  assert.deepEqual(persisted, ["latest editor state"]);
  assert.equal(coordinator.hasPending(), false);
});

test("the active view-navigation bridge flushes and unregisters cleanly", async () => {
  let flushes = 0;
  const unregister = registerSessionSaveFlusher(async () => {
    flushes += 1;
  });

  await flushActiveSessionEdits();
  unregister();
  await flushActiveSessionEdits();

  assert.equal(flushes, 1);
});

test("failed snapshots remain retryable without changing their target", async () => {
  let attempts = 0;
  const persisted: SessionSaveSnapshot[] = [];
  const coordinator = new SessionSaveCoordinator({
    persist: async (snapshot) => {
      attempts += 1;
      if (attempts === 1) throw new Error("temporary database failure");
      persisted.push(snapshot);
    },
  });

  coordinator.schedule({ sessionId: "A", title: "A", content: "safe" });
  await assert.rejects(coordinator.flush("A"), /temporary database failure/);
  assert.equal(coordinator.hasPending("A"), true);
  await coordinator.flush("A");

  assert.equal(persisted[0].sessionId, "A");
  assert.equal(persisted[0].content, "safe");
});

test("session synchronization ignores identical local-save content", () => {
  assert.equal(shouldLoadSessionContent({
    currentHtml: "<p>same</p>",
    incomingHtml: "  <p>same</p> ",
    currentSessionId: "A",
    incomingSessionId: "A",
  }), false);
  assert.equal(shouldLoadSessionContent({
    currentHtml: "<p>before</p>",
    incomingHtml: "<p>external</p>",
    currentSessionId: "A",
    incomingSessionId: "A",
  }), true);
  assert.equal(shouldLoadSessionContent({
    currentHtml: "<p>same</p>",
    incomingHtml: "<p>same</p>",
    currentSessionId: "A",
    incomingSessionId: "B",
  }), true);
});

test("font-size state distinguishes defaults, explicit sizes, and mixed selections", () => {
  assert.equal(resolveFontSizeState([]), "16px");
  assert.equal(resolveFontSizeState([null]), "16px");
  assert.equal(resolveFontSizeState(["24px", "24px"]), "24px");
  assert.equal(resolveFontSizeState(["24px", null]), "mixed");
  assert.equal(resolveFontSizeState(["18px", "24px"]), "mixed");
});

test("selection snapshots are invalidated by content changes or invalid ranges", () => {
  const snapshot = { from: 2, to: 8, documentGeneration: 4 };
  assert.equal(isSelectionSnapshotValid(snapshot, 4, 20), true);
  assert.equal(isSelectionSnapshotValid(snapshot, 5, 20), false);
  assert.equal(isSelectionSnapshotValid(snapshot, 4, 6), false);
});

test("existing structural session HTML remains unchanged", () => {
  const html = [
    "<h2>Heading</h2>",
    "<p><strong>Bold</strong> and <em>italic</em> with ",
    '<span style="font-size: 24px">size</span> and <a href="https://example.com">link</a>.</p>',
    "<ul><li><p>Bullet</p></li></ul>",
    "<ol><li><p>Numbered</p></li></ol>",
    "<blockquote><p>Quotation</p></blockquote>",
  ].join("");
  assert.equal(normalizeSessionHtml(html), html);
});

test("editor commands, loop guard, history reset, and scoped structures are wired", () => {
  const component = readFileSync(new URL("../src/components/SessionsView.tsx", import.meta.url), "utf8");
  const css = readFileSync(new URL("../src/app/globals.css", import.meta.url), "utf8");

  assert.match(component, /toggleBulletList\(\)\.run\(\)/);
  assert.match(component, /toggleOrderedList\(\)\.run\(\)/);
  assert.match(component, /toggleBlockquote\(\)\.run\(\)/);
  assert.match(component, /setContent\(incomingContent, \{ emitUpdate: false \}\)/);
  assert.match(component, /EditorState\.create/);
  assert.match(component, /className="session-editor/);
  assert.match(css, /\.session-editor \.ProseMirror ul/);
  assert.match(css, /list-style-type: disc/);
  assert.match(css, /list-style-type: decimal/);
  assert.match(css, /\.session-editor \.ProseMirror blockquote/);
});

test("session deletion confirmation identifies the session and Cancel performs no delete", async () => {
  let deletes = 0;
  let discarded = 0;
  const result = await runConfirmedSessionDeletion({
    sessionId: "session-a",
    title: "Romans Notes",
    confirm: (message) => {
      assert.equal(message, getSessionDeleteConfirmationMessage("Romans Notes"));
      assert.match(message, /attached documents/);
      assert.match(message, /cannot be undone/);
      return false;
    },
    discardPending: () => {
      discarded += 1;
    },
    restorePending: () => undefined,
    deleteSession: async () => {
      deletes += 1;
    },
  });
  assert.equal(result, false);
  assert.equal(discarded, 0);
  assert.equal(deletes, 0);
});

test("confirmed session deletion cancels pending writes and invokes delete once", async () => {
  const calls: string[] = [];
  const result = await runConfirmedSessionDeletion({
    sessionId: "session-a",
    title: "Romans Notes",
    confirm: () => true,
    discardPending: (sessionId) => calls.push(`discard:${sessionId}`),
    restorePending: (sessionId) => calls.push(`restore:${sessionId}`),
    deleteSession: async (sessionId) => {
      calls.push(`delete:${sessionId}`);
    },
  });
  assert.equal(result, true);
  assert.deepEqual(calls, ["discard:session-a", "delete:session-a"]);
});

test("failed session deletion restores pending-save eligibility", async () => {
  const calls: string[] = [];
  await assert.rejects(runConfirmedSessionDeletion({
    sessionId: "session-a",
    title: "Romans Notes",
    confirm: () => true,
    discardPending: (sessionId) => calls.push(`discard:${sessionId}`),
    restorePending: (sessionId) => calls.push(`restore:${sessionId}`),
    deleteSession: async () => {
      throw new Error("delete failed");
    },
  }), /delete failed/);
  assert.deepEqual(calls, ["discard:session-a", "restore:session-a"]);
});
