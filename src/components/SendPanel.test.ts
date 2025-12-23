import { describe, it, expect } from 'vitest';

/**
 * SendPanel utility function tests
 * These test the core logic without rendering the component
 */

// Extract hex parsing logic for testing
function parseHexString(input: string): number[] | null {
    const cleanHex = input.replace(/\s+/g, '');
    if (cleanHex.length % 2 !== 0) {
        return null; // Invalid: odd number of characters
    }
    const result: number[] = [];
    for (let i = 0; i < cleanHex.length; i += 2) {
        const byte = parseInt(cleanHex.substring(i, i + 2), 16);
        if (isNaN(byte)) return null; // Invalid hex character
        result.push(byte);
    }
    return result;
}

// Extract line ending logic for testing
function appendLineEnding(data: number[], lineEnding: 'NONE' | 'CR' | 'LF' | 'CRLF'): number[] {
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

// Extract history management logic for testing
function addToHistory(history: string[], newItem: string, maxSize: number = 20): string[] {
    // If same as the most recent one, don't add
    if (history.length > 0 && history[0] === newItem) {
        return history;
    }
    return [newItem, ...history].slice(0, maxSize);
}

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
        expect(addToHistory(['first', 'second'], 'second')).toEqual([
            'second',
            'first',
            'second',
        ]);
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
