import { describe, it, expect } from 'vitest';
import { parseHex, parseHexString, appendLineEnding, addToHistory } from '../utils/sendUtils';

/**
 * SendPanel utility function tests
 * Testing the actual sendUtils functions used by SendPanel
 */

describe('SendPanel - Hex Parsing', () => {
  it('parses valid hex string without spaces', () => {
    expect(parseHexString('48656C6C6F')).toEqual([0x48, 0x65, 0x6c, 0x6c, 0x6f]);
  });

  it('parses valid hex string with spaces', () => {
    expect(parseHexString('48 65 6C 6C 6F')).toEqual([0x48, 0x65, 0x6c, 0x6c, 0x6f]);
  });

  it('handles lowercase hex', () => {
    expect(parseHexString('ff00aa')).toEqual([0xff, 0x00, 0xaa]);
  });

  it('handles mixed case hex', () => {
    expect(parseHexString('Ff 00 Aa')).toEqual([0xff, 0x00, 0xaa]);
  });

  it('returns null for odd number of characters', () => {
    expect(parseHexString('48656')).toBeNull();
  });

  it('returns null for invalid hex characters', () => {
    expect(parseHexString('48GG')).toBeNull();
  });

  it('handles empty string', () => {
    expect(parseHexString('')).toEqual([]);
  });

  it('handles single byte', () => {
    expect(parseHexString('FF')).toEqual([0xff]);
  });

  // C# version: split at '-', ' ', ',', '.', '/', each token one byte
  it('accepts the C# separators and one-digit bytes', () => {
    expect(parseHexString('0A-0B')).toEqual([0x0a, 0x0b]);
    expect(parseHexString('1,2,3')).toEqual([0x01, 0x02, 0x03]);
    expect(parseHexString('a.b/c')).toEqual([0x0a, 0x0b, 0x0c]);
    expect(parseHexString('01:02 03\t04\n05')).toEqual([1, 2, 3, 4, 5]);
    expect(parseHexString('41-42-')).toEqual([0x41, 0x42]); // trailing separator
  });

  it('reads one-digit tokens as bytes instead of pairing digits across tokens', () => {
    // Before, "12 3 4" was paired up as "1234" and sent 0x12 0x34
    expect(parseHexString('12 3 4')).toEqual([0x12, 0x03, 0x04]);
  });

  it('accepts a 0x prefix', () => {
    expect(parseHexString('0x41 0X42 0x4344')).toEqual([0x41, 0x42, 0x43, 0x44]);
  });

  it('names the token it cannot read', () => {
    expect(parseHex('41 123 42')).toEqual({ ok: false, token: '123' });
    expect(parseHex('41 G1')).toEqual({ ok: false, token: 'G1' });
    expect(parseHex('0x')).toEqual({ ok: false, token: '0x' });
  });
});

describe('SendPanel - Line Endings', () => {
  it('appends nothing for NONE', () => {
    expect(appendLineEnding([0x41, 0x42], 'NONE')).toEqual([0x41, 0x42]);
  });

  it('appends CR (0x0D) for CR', () => {
    expect(appendLineEnding([0x41], 'CR')).toEqual([0x41, 0x0d]);
  });

  it('appends LF (0x0A) for LF', () => {
    expect(appendLineEnding([0x41], 'LF')).toEqual([0x41, 0x0a]);
  });

  it('appends CRLF (0x0D 0x0A) for CRLF', () => {
    expect(appendLineEnding([0x41], 'CRLF')).toEqual([0x41, 0x0d, 0x0a]);
  });

  it('handles empty array', () => {
    expect(appendLineEnding([], 'LF')).toEqual([0x0a]);
  });
});

describe('SendPanel - History Management', () => {
  it('adds new item to empty history', () => {
    expect(addToHistory([], 'hello')).toEqual(['hello']);
  });

  it('adds new item to front of history', () => {
    expect(addToHistory(['old'], 'new')).toEqual(['new', 'old']);
  });

  it('does not add duplicate if same as most recent', () => {
    expect(addToHistory(['same', 'other'], 'same')).toEqual(['same', 'other']);
  });

  it('allows duplicate if not most recent', () => {
    expect(addToHistory(['first', 'second'], 'second')).toEqual(['second', 'first', 'second']);
  });

  it('limits history size to maxSize', () => {
    const history = Array.from({ length: 20 }, (_, i) => `item${i}`);
    const result = addToHistory(history, 'new', 20);
    expect(result).toHaveLength(20);
    expect(result[0]).toBe('new');
    expect(result[19]).toBe('item18'); // last item dropped
  });

  it('does not remember empty or line-break-only input (C# version rule)', () => {
    expect(addToHistory(['a'], '')).toEqual(['a']);
    expect(addToHistory(['a'], '\n')).toEqual(['a']);
    expect(addToHistory(['a'], '\r\n\r\n')).toEqual(['a']);
  });

  it('remembers whitespace-only input that is not just line breaks', () => {
    expect(addToHistory(['a'], ' ')).toEqual([' ', 'a']);
  });
});
