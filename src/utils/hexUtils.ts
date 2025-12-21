/**
 * Hex/ASCII display utilities for serial data viewer
 */

/**
 * Convert a byte array to a hex string with spaces between bytes
 * @param bytes - The byte array to convert
 * @returns Formatted hex string (e.g., "00 0A 0D 48")
 */
export function bytesToHex(bytes: Uint8Array): string {
    return Array.from(bytes)
        .map((b) => b.toString(16).padStart(2, '0').toUpperCase())
        .join(' ');
}

/**
 * Convert a byte array to an ASCII string with control character visualization
 * @param bytes - The byte array to convert
 * @returns ASCII string with control characters shown as special markers
 */
export function bytesToAscii(bytes: Uint8Array): string {
    return Array.from(bytes)
        .map((b) => {
            if (b >= 0x20 && b <= 0x7e) {
                return String.fromCharCode(b);
            }
            // Control character visualization
            switch (b) {
                case 0x00: return '␀'; // NULL
                case 0x07: return '␇'; // BEL
                case 0x08: return '␈'; // BS
                case 0x09: return '␉'; // TAB
                case 0x0a: return '␊'; // LF
                case 0x0d: return '␍'; // CR
                case 0x1b: return '␛'; // ESC
                case 0x7f: return '␡'; // DEL
                default: return '·'; // Non-printable
            }
        })
        .join('');
}

/**
 * Format an offset number as a hex address
 * @param offset - The byte offset
 * @param padLength - Number of hex digits (default 8)
 * @returns Formatted address string (e.g., "00000000")
 */
export function formatOffset(offset: number, padLength: number = 8): string {
    return offset.toString(16).toUpperCase().padStart(padLength, '0');
}

/**
 * Split byte data into rows for hex viewer display
 * @param data - The byte array to split
 * @param bytesPerRow - Number of bytes per row (default 16)
 * @returns Array of row objects with offset and data
 */
export function splitIntoRows(
    data: Uint8Array,
    bytesPerRow: number = 16
): { offset: number; data: Uint8Array }[] {
    const rows: { offset: number; data: Uint8Array }[] = [];

    for (let i = 0; i < data.length; i += bytesPerRow) {
        rows.push({
            offset: i,
            data: data.slice(i, Math.min(i + bytesPerRow, data.length)),
        });
    }

    return rows;
}

/**
 * Calculate the number of rows needed for a given total byte count
 * @param totalBytes - Total bytes in the data
 * @param bytesPerRow - Number of bytes per row (default 16)
 * @returns Number of rows
 */
export function calculateRowCount(totalBytes: number, bytesPerRow: number = 16): number {
    return Math.ceil(totalBytes / bytesPerRow);
}
