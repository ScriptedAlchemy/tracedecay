/**
 * Token-level overlap diff for the similarity pair cards.
 *
 * Splits each text into word/whitespace runs, normalizes word tokens, and
 * marks each word as `shared` (its normalized form appears in the other text)
 * or `unique`. Whitespace/punctuation runs pass through unmarked so the
 * original text renders verbatim.
 */

export interface DiffToken {
  text: string;
  kind: "shared" | "unique" | "plain";
}

const WORD_RE = /[\p{L}\p{N}_][\p{L}\p{N}_'-]*/gu;

function normalize(token: string): string {
  return token.toLowerCase().replace(/['-]/g, "");
}

function wordSet(text: string): Set<string> {
  const set = new Set<string>();
  for (const match of text.matchAll(WORD_RE)) set.add(normalize(match[0]));
  return set;
}

function tokenize(text: string, other: Set<string>): DiffToken[] {
  const out: DiffToken[] = [];
  let last = 0;
  for (const match of text.matchAll(WORD_RE)) {
    const idx = match.index ?? 0;
    if (idx > last) out.push({ text: text.slice(last, idx), kind: "plain" });
    out.push({
      text: match[0],
      kind: other.has(normalize(match[0])) ? "shared" : "unique",
    });
    last = idx + match[0].length;
  }
  if (last < text.length) out.push({ text: text.slice(last), kind: "plain" });
  return out;
}

export interface PairDiff {
  a: DiffToken[];
  b: DiffToken[];
  /** Jaccard overlap of the normalized word sets, 0..1. */
  jaccard: number;
}

export function diffPair(aText: string, bText: string): PairDiff {
  const aWords = wordSet(aText);
  const bWords = wordSet(bText);
  let shared = 0;
  for (const word of aWords) if (bWords.has(word)) shared += 1;
  const unionSize = aWords.size + bWords.size - shared;
  return {
    a: tokenize(aText, bWords),
    b: tokenize(bText, aWords),
    jaccard: unionSize > 0 ? shared / unionSize : 0,
  };
}
