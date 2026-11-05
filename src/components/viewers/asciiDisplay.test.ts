import { describe, it, expect } from 'vitest';
import { asciiDisplaySegments } from './asciiDisplay';

// Control Pictures as sent by the backend
const NUL = '␀';
const TAB = '␉';
const LF = '␊';
const CR = '␍';

const visible = (text: string, showCrLf: boolean, showNul: boolean) =>
  asciiDisplaySegments(text, { showCrLf, showNul })
    .filter((s) => !s.hidden)
    .map((s) => s.text)
    .join('');

describe('asciiDisplaySegments', () => {
  it('shows everything as symbols when both options are on', () => {
    const text = `a${NUL}b${TAB}c${CR}${LF}`;
    expect(asciiDisplaySegments(text, { showCrLf: true, showNul: true })).toEqual([
      { text, hidden: false },
    ]);
  });

  it('hides CR/LF but keeps them for copying when Show CR/LF is off', () => {
    expect(asciiDisplaySegments(`temp=25${CR}${LF}`, { showCrLf: false, showNul: true })).toEqual([
      { text: 'temp=25', hidden: false },
      { text: `${CR}${LF}`, hidden: true },
    ]);
  });

  it('shows TAB as a real tab when Show CR/LF is off', () => {
    expect(visible(`a${TAB}b${LF}`, false, false)).toBe('a\tb');
    expect(visible(`a${TAB}b${LF}`, true, false)).toBe(`a${TAB}b${LF}`);
  });

  it('removes NUL when Show NUL is off', () => {
    expect(visible(`${NUL}a${NUL}${NUL}b`, true, false)).toBe('ab');
    expect(visible(`${NUL}a`, true, true)).toBe(`${NUL}a`);
  });

  it('handles a line made only of a line break', () => {
    expect(asciiDisplaySegments(LF, { showCrLf: false, showNul: false })).toEqual([
      { text: LF, hidden: true },
    ]);
    expect(asciiDisplaySegments('', { showCrLf: false, showNul: false })).toEqual([]);
  });

  it('leaves other control symbols visible', () => {
    expect(visible('␛[0m', false, false)).toBe('␛[0m'); // ESC
  });
});
