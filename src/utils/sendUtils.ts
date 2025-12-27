/**
 * Utility functions for SendPanel
 * Extracted for testability and reusability
 */

export type LineEnding = 'NONE' | 'CR' | 'LF' | 'CRLF';

/**
 * Parse a hex string into an array of bytes.
 * Spaces are ignored. Returns null if invalid.
 * 
 * @param input - Hex string like "48 65 6C 6C 6F" or "48656C6C6F"
 * @returns Array of bytes or null if invalid
 */
export function parseHexString(input: string): number[] | null {
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
 * Add an item to history, avoiding duplicates of the most recent item.
 * 
 * @param history - Current history array (most recent first)
 * @param newItem - Item to add
 * @param maxSize - Maximum history size (default 20)
 * @returns New history array
 */
export function addToHistory(history: string[], newItem: string, maxSize: number = 20): string[] {
    // If same as the most recent one, don't add
    if (history.length > 0 && history[0] === newItem) {
        return history;
    }
    return [newItem, ...history].slice(0, maxSize);
}
