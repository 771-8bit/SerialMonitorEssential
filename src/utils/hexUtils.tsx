/**
 * Hex/ASCII display utilities for serial data viewer
 */

import React from 'react';

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

// Control character labels indexed by Unicode Control Pictures code points
// U+2400-U+241F (0x2400-0x241F) for control chars 0-31
// U+2421 (0x2421) for DEL (127)
export const CONTROL_LABELS: Record<number, string> = {
  0x2400: 'NUL',
  0x2401: 'SOH',
  0x2402: 'STX',
  0x2403: 'ETX',
  0x2404: 'EOT',
  0x2405: 'ENQ',
  0x2406: 'ACK',
  0x2407: 'BEL',
  0x2408: 'BS',
  0x2409: 'TAB',
  0x240a: 'LF',
  0x240b: 'VT',
  0x240c: 'FF',
  0x240d: 'CR',
  0x240e: 'SO',
  0x240f: 'SI',
  0x2410: 'DLE',
  0x2411: 'DC1',
  0x2412: 'DC2',
  0x2413: 'DC3',
  0x2414: 'DC4',
  0x2415: 'NAK',
  0x2416: 'SYN',
  0x2417: 'ETB',
  0x2418: 'CAN',
  0x2419: 'EM',
  0x241a: 'SUB',
  0x241b: 'ESC',
  0x241c: 'FS',
  0x241d: 'GS',
  0x241e: 'RS',
  0x241f: 'US',
  0x2421: 'DEL',
};

// Map Unicode Control Pictures to actual control characters for copying
const CONTROL_PICTURE_TO_CHAR: Record<number, string> = {
  0x2400: '\x00', // NUL
  0x2401: '\x01', // SOH
  0x2402: '\x02', // STX
  0x2403: '\x03', // ETX
  0x2404: '\x04', // EOT
  0x2405: '\x05', // ENQ
  0x2406: '\x06', // ACK
  0x2407: '\x07', // BEL
  0x2408: '\x08', // BS
  0x2409: '\x09', // TAB
  0x240a: '\x0a', // LF
  0x240b: '\x0b', // VT
  0x240c: '\x0c', // FF
  0x240d: '\x0d', // CR
  0x240e: '\x0e', // SO
  0x240f: '\x0f', // SI
  0x2410: '\x10', // DLE
  0x2411: '\x11', // DC1
  0x2412: '\x12', // DC2
  0x2413: '\x13', // DC3
  0x2414: '\x14', // DC4
  0x2415: '\x15', // NAK
  0x2416: '\x16', // SYN
  0x2417: '\x17', // ETB
  0x2418: '\x18', // CAN
  0x2419: '\x19', // EM
  0x241a: '\x1a', // SUB
  0x241b: '\x1b', // ESC
  0x241c: '\x1c', // FS
  0x241d: '\x1d', // GS
  0x241e: '\x1e', // RS
  0x241f: '\x1f', // US
  0x2421: '\x7f', // DEL
};

// Middle dot character used by backend for non-ASCII bytes
export const MIDDLE_DOT = '·';

/**
 * Render ASCII text with control character labels for HexViewer
 * Optimized: batch consecutive regular characters to reduce span count
 */
export function renderAsciiColumn(text: string): React.ReactNode {
  const elements: React.ReactNode[] = [];
  let i = 0;

  while (i < text.length) {
    const code = text.charCodeAt(i);
    const label = CONTROL_LABELS[code];

    if (label) {
      // Unicode Control Picture - show as small label
      elements.push(
        <span key={i} className="ascii-char ascii-ctrl" title={label}>
          {label}
        </span>
      );
      i++;
    } else if (text[i] === MIDDLE_DOT) {
      // Non-ASCII byte (shown as middle dot by backend)
      elements.push(
        <span key={i} className="ascii-char">
          ·
        </span>
      );
      i++;
    } else {
      // Regular ASCII character - render in fixed-width cell like control chars
      elements.push(
        <span key={i} className="ascii-char">
          {text[i]}
        </span>
      );
      i++;
    }
  }
  return <>{elements}</>;
}

/**
 * Render ASCII line for AsciiViewer with copyable control characters
 * Control characters are rendered with actual char (for copy) + visual label overlay
 */
export function renderAsciiLine(text: string): React.ReactNode {
  const elements: React.ReactNode[] = [];
  let i = 0;

  while (i < text.length) {
    const code = text.charCodeAt(i);
    const label = CONTROL_LABELS[code];
    const actualChar = CONTROL_PICTURE_TO_CHAR[code];

    if (label && actualChar) {
      // Unicode Control Picture - render with actual control char for copying
      elements.push(
        <span key={i} className="ascii-ctrl-char" title={label} data-label={label}>
          {actualChar}
        </span>
      );
    } else if (text[i] === MIDDLE_DOT) {
      // Non-ASCII byte
      elements.push(<span key={i}>·</span>);
    } else {
      // Regular character - batch consecutive regular chars
      let end = i + 1;
      while (end < text.length) {
        const nextCode = text.charCodeAt(end);
        if (CONTROL_LABELS[nextCode] || text[end] === MIDDLE_DOT) break;
        end++;
      }
      elements.push(<span key={i}>{text.slice(i, end)}</span>);
      i = end;
      continue;
    }
    i++;
  }
  return <>{elements}</>;
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
