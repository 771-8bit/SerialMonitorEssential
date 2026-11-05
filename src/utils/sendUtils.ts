/**
 * Utility functions for SendPanel
 * Extracted for testability and reusability
 */

export type LineEnding = 'NONE' | 'CR' | 'LF' | 'CRLF';

export type HexParseResult = { ok: true; bytes: number[] } | { ok: false; token: string };

/** Characters that separate bytes, as in the C# version (plus tab/newline and ':') */
const HEX_SEPARATORS = /[\s,\-./:]+/;

/**
 * Parse hex input into bytes, as the C# version did: the input is split at spaces,
 * '-', ',', '.', '/' (and ':'), and every token is one byte of 1 or 2 hex digits, with
 * an optional 0x prefix ("0A-0B", "1,2,3", "0x41 0x42"). A token with an even number
 * of digits above 2 is read as consecutive bytes ("48656C6C6F").
 *
 * Any other token is rejected and returned, so the caller can name it: "123" is
 * ambiguous (0x12 0x03 or 0x01 0x23) and is never guessed.
 */
export function parseHex(input: string): HexParseResult {
  const bytes: number[] = [];
  for (const raw of input.split(HEX_SEPARATORS)) {
    if (raw === '') continue;
    const digits = /^0[xX]/.test(raw) ? raw.slice(2) : raw;
    if (!/^[0-9a-fA-F]+$/.test(digits) || (digits.length > 2 && digits.length % 2 !== 0)) {
      return { ok: false, token: raw };
    }
    if (digits.length <= 2) {
      bytes.push(parseInt(digits, 16));
    } else {
      for (let i = 0; i < digits.length; i += 2) {
        bytes.push(parseInt(digits.substring(i, i + 2), 16));
      }
    }
  }
  return { ok: true, bytes };
}

/**
 * Parse hex input into bytes (see parseHex); null if any token is invalid.
 *
 * @param input - Hex string like "48 65 6C 6C 6F", "48656C6C6F" or "0A-0B"
 */
export function parseHexString(input: string): number[] | null {
  const result = parseHex(input);
  return result.ok ? result.bytes : null;
}

/**
 * Append line ending bytes to data array.
 *
 * @param data - Array of bytes
 * @param lineEnding - Line ending type
 * @returns New array with line ending appended
 */
export function appendLineEnding(data: number[], lineEnding: LineEnding): number[] {
  const result = [...data];
  switch (lineEnding) {
    case 'CR':
      result.push(0x0d);
      break;
    case 'LF':
      result.push(0x0a);
      break;
    case 'CRLF':
      result.push(0x0d, 0x0a);
      break;
  }
  return result;
}

/**
 * Add an item to history, avoiding duplicates of the most recent item and
 * line-break-only input.
 *
 * @param history - Current history array (most recent first)
 * @param newItem - Item to add
 * @param maxSize - Maximum history size (default 20)
 * @returns New history array
 */
export function addToHistory(history: string[], newItem: string, maxSize: number = 20): string[] {
  // Input that is empty once line breaks are removed is sent but not remembered
  // (same rule as the C# version)
  if (newItem.replace(/[\r\n]/g, '') === '') {
    return history;
  }
  // If same as the most recent one, don't add
  if (history.length > 0 && history[0] === newItem) {
    return history;
  }
  return [newItem, ...history].slice(0, maxSize);
}
