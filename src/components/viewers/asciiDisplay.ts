/**
 * How a received line is shown in the ASCII view (C# version: "Show [CRLF]" and
 * "Show [NUL]", both off by default).
 *
 * The backend sends every control byte as a Unicode Control Picture (U+2400 + byte).
 * - Show CR/LF off: CR and LF are hidden and TAB is shown as a real tab. The hidden
 *   CR/LF stay in the DOM (in a zero-size span) so that copying a selection, which
 *   turns the pictures back into bytes, still produces the line breaks.
 * - Show NUL off: NUL is removed from the view (and from a copied selection), as the
 *   C# version did.
 */

export interface AsciiDisplayOptions {
  showCrLf: boolean;
  showNul: boolean;
}

export interface AsciiSegment {
  text: string;
  /** Kept for copying, but not visible */
  hidden: boolean;
}

const NUL_PICTURE = /␀/g;
const TAB_PICTURE = /␉/g;
const CR_LF_RUN = /([␊␍]+)/;

export function asciiDisplaySegments(text: string, options: AsciiDisplayOptions): AsciiSegment[] {
  let shown = options.showNul ? text : text.replace(NUL_PICTURE, '');
  if (options.showCrLf) {
    return [{ text: shown, hidden: false }];
  }
  shown = shown.replace(TAB_PICTURE, '\t');
  // split() with a capture group alternates: visible, CR/LF run, visible, ...
  return shown
    .split(CR_LF_RUN)
    .map((part, i) => ({ text: part, hidden: i % 2 === 1 }))
    .filter((segment) => segment.text !== '');
}
