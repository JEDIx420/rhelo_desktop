"use client";

import { useRef, useState } from "react";
import { Check, Loader2, RotateCcw, SearchCheck, X } from "lucide-react";

import {
  suggestBiblicalTerms,
  type BiblicalSuggestionCandidate,
  type BiblicalTermSuggestion,
  type SessionRecord,
} from "@/lib/api";
import {
  applyBiblicalSuggestion,
  ignoreBiblicalSuggestion,
} from "@/lib/transcriptReview";

interface TranscriptReviewModalProps {
  transcript: string;
  sessions: SessionRecord[];
  targetSessionId: string | null;
  onTranscriptChange: (transcript: string) => void;
  onTargetSessionChange: (sessionId: string) => void;
  onDiscard: () => void;
  onAdd: () => void;
}

interface CorrectionUndo {
  transcript: string;
  suggestions: BiblicalTermSuggestion[];
}

export default function TranscriptReviewModal({
  transcript,
  sessions,
  targetSessionId,
  onTranscriptChange,
  onTargetSessionChange,
  onDiscard,
  onAdd,
}: TranscriptReviewModalProps) {
  const [suggestions, setSuggestions] = useState<BiblicalTermSuggestion[]>([]);
  const [selectedCandidates, setSelectedCandidates] = useState<Record<number, number>>({});
  const [checking, setChecking] = useState(false);
  const [correctionError, setCorrectionError] = useState<string | null>(null);
  const [correctionMessage, setCorrectionMessage] = useState<string | null>(null);
  const [undo, setUndo] = useState<CorrectionUndo | null>(null);
  const correctionRequestRef = useRef(0);

  const updateTranscript = (value: string) => {
    correctionRequestRef.current += 1;
    setSuggestions([]);
    setSelectedCandidates({});
    setCorrectionError(null);
    setCorrectionMessage(null);
    setUndo(null);
    onTranscriptChange(value);
  };

  const checkBiblicalTerms = async () => {
    const requestId = correctionRequestRef.current + 1;
    correctionRequestRef.current = requestId;
    const requestedTranscript = transcript;
    setChecking(true);
    setCorrectionError(null);
    setCorrectionMessage(null);
    setUndo(null);

    try {
      const response = await suggestBiblicalTerms(requestedTranscript);
      if (requestId !== correctionRequestRef.current || requestedTranscript !== transcript) return;
      setSuggestions(response.suggestions);
      setSelectedCandidates({});
      if (!response.supported) {
        setCorrectionMessage("Suggestions currently support English and Latin-script transcripts only.");
      } else if (response.suggestions.length === 0) {
        setCorrectionMessage("No strong Biblical spelling suggestions were found.");
      } else {
        setCorrectionMessage(
          `${response.suggestions.length} possible ${
            response.suggestions.length === 1 ? "correction" : "corrections"
          } found locally.`,
        );
      }
    } catch {
      if (requestId === correctionRequestRef.current) {
        setCorrectionError("Rhelo could not check the local Biblical vocabulary. Your transcript was not changed.");
      }
    } finally {
      if (requestId === correctionRequestRef.current) setChecking(false);
    }
  };

  const replaceSuggestion = (
    suggestionIndex: number,
    candidate: BiblicalSuggestionCandidate,
  ) => {
    setUndo({ transcript, suggestions });
    const result = applyBiblicalSuggestion(
      transcript,
      suggestions,
      suggestionIndex,
      candidate,
    );
    onTranscriptChange(result.transcript);
    setSuggestions(result.suggestions);
    setSelectedCandidates({});
  };

  const ignoreSuggestion = (suggestionIndex: number) => {
    setSuggestions((current) => ignoreBiblicalSuggestion(current, suggestionIndex));
    setSelectedCandidates({});
  };

  const undoCorrection = () => {
    if (!undo) return;
    onTranscriptChange(undo.transcript);
    setSuggestions(undo.suggestions);
    setSelectedCandidates({});
    setUndo(null);
  };

  return (
    <div className="fixed inset-0 z-[3000] flex items-center justify-center bg-slate-900/35 p-6 backdrop-blur-xs">
      <div
        role="dialog"
        aria-modal="true"
        aria-labelledby="transcript-review-title"
        className="z-[3010] flex max-h-[90vh] w-full max-w-2xl flex-col gap-4 rounded-3xl border border-slate-200 bg-white p-6 font-sans shadow-2xl"
      >
        <div className="flex items-center justify-between border-b border-slate-100 pb-3">
          <div>
            <h4 id="transcript-review-title" className="text-lg font-extrabold text-slate-900">
              Review transcript
            </h4>
            <p className="mt-1 text-xs text-slate-500">
              Edit everything before adding it to your study session.
            </p>
          </div>
          <button
            type="button"
            onClick={onDiscard}
            aria-label="Discard transcript"
            className="rounded-lg p-2 text-slate-400 hover:bg-slate-100 hover:text-slate-700"
          >
            <X size={18} />
          </button>
        </div>

        <div className="flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto pr-1">
          <div className="flex flex-col gap-2">
            <label htmlFor="transcript-review-text" className="text-xs font-bold uppercase tracking-wider text-slate-500">
              Transcript draft
            </label>
            <textarea
              id="transcript-review-text"
              value={transcript}
              onChange={(event) => updateTranscript(event.target.value)}
              className="min-h-36 w-full resize-y rounded-xl border border-slate-200 p-3 text-sm leading-relaxed text-slate-800 focus:border-blue-500 focus:outline-none focus:ring-2 focus:ring-blue-500/20"
              placeholder="Captured speech will appear here..."
            />
          </div>

          <div className="rounded-xl border border-blue-100 bg-blue-50/60 p-3">
            <div className="flex flex-wrap items-center justify-between gap-3">
              <p className="max-w-md text-xs leading-relaxed text-blue-950">
                Suggestions use Rhelo&apos;s offline Biblical names, people, places, books, and terms.
                Nothing is sent to an online correction service.
              </p>
              <button
                type="button"
                onClick={() => void checkBiblicalTerms()}
                disabled={checking || !transcript.trim()}
                className="flex items-center gap-2 rounded-lg border border-blue-200 bg-white px-3 py-2 text-xs font-bold text-blue-700 hover:bg-blue-50 disabled:opacity-50"
              >
                {checking ? <Loader2 size={14} className="animate-spin" /> : <SearchCheck size={14} />}
                {checking ? "Checking locally..." : "Check Biblical names and terms"}
              </button>
            </div>
            {correctionError ? <p role="alert" className="mt-2 text-xs font-semibold text-red-700">{correctionError}</p> : null}
            {correctionMessage ? <p aria-live="polite" className="mt-2 text-xs font-semibold text-blue-800">{correctionMessage}</p> : null}
          </div>

          {suggestions.length > 0 ? (
            <div className="space-y-2" aria-label="Possible Biblical spelling corrections">
              {suggestions.map((suggestion, suggestionIndex) => {
                const candidateIndex = selectedCandidates[suggestionIndex] ?? 0;
                const candidate = suggestion.candidates[candidateIndex] || suggestion.candidates[0];
                return (
                  <div key={`${suggestion.start}-${suggestion.original}`} className="rounded-xl border border-slate-200 p-3">
                    <div className="flex flex-wrap items-center gap-2">
                      <span className="rounded-md bg-red-50 px-2 py-1 text-sm font-semibold text-red-800 line-through decoration-red-300">
                        {suggestion.original}
                      </span>
                      <span aria-hidden="true" className="text-slate-400">→</span>
                      {suggestion.candidates.length > 1 ? (
                        <select
                          aria-label={`Replacement for ${suggestion.original}`}
                          value={candidateIndex}
                          onChange={(event) => setSelectedCandidates((current) => ({
                            ...current,
                            [suggestionIndex]: Number(event.target.value),
                          }))}
                          className="rounded-lg border border-slate-200 bg-white px-2 py-1 text-sm font-semibold text-emerald-800"
                        >
                          {suggestion.candidates.map((option, index) => (
                            <option key={`${option.term}-${option.category}`} value={index}>
                              {option.term} · {option.category}
                            </option>
                          ))}
                        </select>
                      ) : (
                        <span className="rounded-md bg-emerald-50 px-2 py-1 text-sm font-semibold text-emerald-800">
                          {candidate.term}
                        </span>
                      )}
                      <span className="text-[11px] font-bold uppercase tracking-wide text-slate-400">
                        {candidate.category}
                      </span>
                      <div className="ml-auto flex items-center gap-2">
                        <button
                          type="button"
                          onClick={() => ignoreSuggestion(suggestionIndex)}
                          className="rounded-lg px-2.5 py-1.5 text-xs font-semibold text-slate-600 hover:bg-slate-100"
                        >
                          Ignore
                        </button>
                        <button
                          type="button"
                          onClick={() => replaceSuggestion(suggestionIndex, candidate)}
                          className="flex items-center gap-1 rounded-lg bg-emerald-600 px-2.5 py-1.5 text-xs font-bold text-white hover:bg-emerald-700"
                        >
                          <Check size={13} />
                          Replace
                        </button>
                      </div>
                    </div>
                  </div>
                );
              })}
            </div>
          ) : null}

          {undo ? (
            <button
              type="button"
              onClick={undoCorrection}
              className="flex w-fit items-center gap-2 rounded-lg px-2 py-1 text-xs font-semibold text-slate-600 hover:bg-slate-100"
            >
              <RotateCcw size={13} />
              Undo last correction
            </button>
          ) : null}

          <div className="flex flex-col gap-2">
            <label htmlFor="transcript-target-session" className="text-xs font-bold uppercase tracking-wider text-slate-500">
              Target study session
            </label>
            <select
              id="transcript-target-session"
              value={targetSessionId || ""}
              onChange={(event) => onTargetSessionChange(event.target.value)}
              className="w-full rounded-xl border border-slate-200 bg-white p-3 text-sm text-slate-850 shadow-sm focus:border-blue-500 focus:outline-none focus:ring-2 focus:ring-blue-500/20"
            >
              {sessions.length === 0 ? (
                <option value="">No sessions available</option>
              ) : sessions.map((session) => (
                <option key={session.session_id} value={session.session_id}>
                  {session.title}
                </option>
              ))}
            </select>
          </div>
        </div>

        <div className="flex items-center justify-end gap-3 border-t border-slate-100 pt-3">
          <button
            type="button"
            onClick={onDiscard}
            className="rounded-xl border border-slate-200 px-4 py-2.5 text-xs font-bold text-slate-600 hover:bg-slate-50 hover:text-slate-900"
          >
            Discard
          </button>
          <button
            type="button"
            onClick={onAdd}
            disabled={!targetSessionId || !transcript.trim()}
            className="rounded-xl bg-blue-600 px-5 py-2.5 text-xs font-bold text-white hover:bg-blue-700 disabled:opacity-50"
          >
            Add to session
          </button>
        </div>
      </div>
    </div>
  );
}
