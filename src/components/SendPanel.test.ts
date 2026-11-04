import { describe, it, expect } from 'vitest';
import { parseHexString, appendLineEnding, addToHistory } from '../utils/sendUtils';

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

  it('handles empty string as valid item', () => {
    expect(addToHistory(['a'], '')).toEqual(['', 'a']);
  });
});
