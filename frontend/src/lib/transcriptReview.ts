import type {
  BiblicalSuggestionCandidate,
  BiblicalTermSuggestion,
} from "@/lib/api";

export function escapePlainText(value: string): string {
  return value
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#39;");
}

export function buildTranscriptAppendHtml(transcript: string, timestamp: string): string {
  const lines = transcript.trim().replaceAll("\r\n", "\n").replaceAll("\r", "\n").split("\n");
  return lines.map((line, index) => {
    const safeLine = line ? escapePlainText(line) : "<br>";
    const prefix = index === 0
      ? `<strong>[${escapePlainText(timestamp)}]</strong>: `
      : "";
    return `<p>${prefix}${safeLine}</p>`;
  }).join("");
}

export function preserveTokenCapitalization(original: string, replacement: string): string {
  if (original === original.toUpperCase()) return replacement.toUpperCase();
  if (original === original.toLowerCase()) return replacement.toLowerCase();
  if (
    original.length > 0
    && original[0] === original[0].toUpperCase()
    && original.slice(1) === original.slice(1).toLowerCase()
  ) {
    return replacement.charAt(0).toUpperCase() + replacement.slice(1).toLowerCase();
  }
  return replacement;
}

export interface CorrectionApplication {
  transcript: string;
  suggestions: BiblicalTermSuggestion[];
}

export function applyBiblicalSuggestion(
  transcript: string,
  suggestions: BiblicalTermSuggestion[],
  suggestionIndex: number,
  candidate: BiblicalSuggestionCandidate,
): CorrectionApplication {
  const suggestion = suggestions[suggestionIndex];
  if (!suggestion) return { transcript, suggestions };
  if (transcript.slice(suggestion.start, suggestion.end) !== suggestion.original) {
    return { transcript, suggestions: [] };
  }

  const replacement = preserveTokenCapitalization(suggestion.original, candidate.term);
  const updatedTranscript = transcript.slice(0, suggestion.start)
    + replacement
    + transcript.slice(suggestion.end);
  const offsetDelta = replacement.length - (suggestion.end - suggestion.start);

  const updatedSuggestions = suggestions.flatMap((item, index) => {
    if (index === suggestionIndex) return [];
    if (item.start < suggestion.end && item.end > suggestion.start) return [];
    if (item.start >= suggestion.end) {
      return [{
        ...item,
        start: item.start + offsetDelta,
        end: item.end + offsetDelta,
      }];
    }
    return [item];
  });

  return { transcript: updatedTranscript, suggestions: updatedSuggestions };
}

export function ignoreBiblicalSuggestion(
  suggestions: BiblicalTermSuggestion[],
  suggestionIndex: number,
): BiblicalTermSuggestion[] {
  return suggestions.filter((_, index) => index !== suggestionIndex);
}
