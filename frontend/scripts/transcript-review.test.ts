import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import type {
  BiblicalSuggestionCandidate,
  BiblicalTermSuggestion,
} from "../src/lib/api";
import {
  applyBiblicalSuggestion,
  buildTranscriptAppendHtml,
  escapePlainText,
  ignoreBiblicalSuggestion,
  preserveTokenCapitalization,
} from "../src/lib/transcriptReview";

const candidate = (
  term: string,
  category: BiblicalSuggestionCandidate["category"],
): BiblicalSuggestionCandidate => ({
  term,
  category,
  distance: 1,
  rank: 100,
});

test("transcript HTML treats angle brackets and script input as literal text", () => {
  const html = buildTranscriptAppendHtml('<script>alert("x")</script>', "10:30 AM");
  assert.doesNotMatch(html, /<script>/);
  assert.match(html, /&lt;script&gt;alert\(&quot;x&quot;\)&lt;\/script&gt;/);
});

test("ampersands, quotes, and apostrophes remain safe", () => {
  assert.equal(
    escapePlainText(`Rock & "roll" isn't <markup>`),
    "Rock &amp; &quot;roll&quot; isn&#39;t &lt;markup&gt;",
  );
});

test("multiline transcripts become fixed paragraphs without changing existing HTML", () => {
  const existing = "<h2>Existing heading</h2><p><strong>Formatted note</strong></p>";
  const appended = buildTranscriptAppendHtml("First line\n\nThird line", "9:05 PM");
  const combined = existing + appended;
  assert.ok(combined.startsWith(existing));
  assert.equal(
    appended,
    "<p><strong>[9:05 PM]</strong>: First line</p><p><br></p><p>Third line</p>",
  );
});

test("suggestions do not modify the draft until Replace is invoked", () => {
  const transcript = "Nebuchadnezar spoke.";
  const suggestions: BiblicalTermSuggestion[] = [{
    original: "Nebuchadnezar",
    start: 0,
    end: 13,
    candidates: [candidate("Nebuchadnezzar", "Person")],
  }];
  assert.equal(transcript, "Nebuchadnezar spoke.");
  assert.equal(suggestions[0].original, "Nebuchadnezar");

  const replaced = applyBiblicalSuggestion(transcript, suggestions, 0, suggestions[0].candidates[0]);
  assert.equal(replaced.transcript, "Nebuchadnezzar spoke.");
  assert.deepEqual(replaced.suggestions, []);
});

test("replacement preserves punctuation, capitalization, and later offsets", () => {
  const transcript = '"capernaun," then THESALONIANS.';
  const suggestions: BiblicalTermSuggestion[] = [
    {
      original: "capernaun",
      start: 1,
      end: 10,
      candidates: [candidate("Capernaum", "Place")],
    },
    {
      original: "THESALONIANS",
      start: 18,
      end: 30,
      candidates: [candidate("Thessalonians", "Bible book")],
    },
  ];

  const first = applyBiblicalSuggestion(transcript, suggestions, 0, suggestions[0].candidates[0]);
  assert.equal(first.transcript, '"capernaum," then THESALONIANS.');
  assert.equal(first.suggestions[0].start, 18);
  const second = applyBiblicalSuggestion(
    first.transcript,
    first.suggestions,
    0,
    first.suggestions[0].candidates[0],
  );
  assert.equal(second.transcript, '"capernaum," then THESSALONIANS.');
});

test("stale offsets cannot replace the wrong text", () => {
  const suggestions: BiblicalTermSuggestion[] = [{
    original: "Capernaun",
    start: 0,
    end: 9,
    candidates: [candidate("Capernaum", "Place")],
  }];
  const result = applyBiblicalSuggestion("Edited Capernaun", suggestions, 0, suggestions[0].candidates[0]);
  assert.equal(result.transcript, "Edited Capernaun");
  assert.deepEqual(result.suggestions, []);
});

test("Ignore removes only the selected issue", () => {
  const suggestions: BiblicalTermSuggestion[] = [
    { original: "one", start: 0, end: 3, candidates: [candidate("One", "Biblical term")] },
    { original: "two", start: 4, end: 7, candidates: [candidate("Two", "Biblical term")] },
  ];
  assert.deepEqual(ignoreBiblicalSuggestion(suggestions, 0), [suggestions[1]]);
});

test("capitalization helpers preserve common speech-recognition casing", () => {
  assert.equal(preserveTokenCapitalization("capernaun", "Capernaum"), "capernaum");
  assert.equal(preserveTokenCapitalization("Capernaun", "Capernaum"), "Capernaum");
  assert.equal(preserveTokenCapitalization("CAPERNAUN", "Capernaum"), "CAPERNAUM");
});

test("review UI is user-invoked, local, editable, and approval-based", () => {
  const component = readFileSync(
    new URL("../src/components/TranscriptReviewModal.tsx", import.meta.url),
    "utf8",
  );
  const page = readFileSync(new URL("../src/app/page.tsx", import.meta.url), "utf8");
  assert.match(component, /Review transcript/);
  assert.match(component, /Check Biblical names and terms/);
  assert.match(component, /offline Biblical names/);
  assert.match(component, /onChange=\{\(event\) => updateTranscript/);
  assert.match(component, />\s*Replace\s*</);
  assert.match(component, />\s*Ignore\s*</);
  assert.match(component, /Add to session/);
  assert.match(page, /buildTranscriptAppendHtml\(transcribedText, timestamp\)/);
  assert.doesNotMatch(page, /\$\{transcribedText\.trim\(\)\}<\/p>/);
});
