import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, waitFor } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import AsciiViewer from '../components/viewers/AsciiViewer';
import { TAIL_START_LINE } from '../components/viewers/scrollUtils';

const invokeMock = vi.mocked(invoke);

// jsdom has no ResizeObserver; the scroll hook constructs one.
class MockResizeObserver {
  observe() {}
  unobserve() {}
  disconnect() {}
}
globalThis.ResizeObserver = MockResizeObserver as unknown as typeof ResizeObserver;

function lines(start: number, count: number) {
  return Array.from({ length: count }, (_, i) => ({
    offset: (start + i) * 10,
    text: `line ${start + i}`,
    timestamp: null,
  }));
}

describe('AsciiViewer - Auto Scroll reaches the last line (issue #3)', () => {
  beforeEach(() => {
    invokeMock.mockReset();
    invokeMock.mockImplementation(async (cmd, args) => {
      if (cmd === 'get_ascii_lines') {
        const a = args as { startLine: number; lineCount: number };
        // As the backend: after a trailing LF the index counts an empty last
        // line that is not sent, so lines.length < total_lines - start
        const total = 1001;
        const start = Math.min(a.startLine, Math.max(0, total - a.lineCount));
        const served = Math.min(a.lineCount, total - start);
        return {
          lines: lines(start, Math.min(served, total - 1 - start)),
          total_lines: total,
          start_line: start,
          reaches_end: start + served >= total,
        };
      }
      return undefined;
    });
  });

  it('asks for the tail and lays the rows out bottom-anchored', async () => {
    const { container, findByText } = render(
      <AsciiViewer totalBytes={10_000} autoScroll={true} lineWrap={true} />
    );

    expect(await findByText('line 999')).toBeInTheDocument();
    expect(invokeMock).toHaveBeenCalledWith(
      'get_ascii_lines',
      expect.objectContaining({ startLine: TAIL_START_LINE })
    );
    // Wrapped rows can be taller than the fixed-row model: the spacer must grow
    // (min-height + flex-end) rather than let the last rows overflow below it
    const spacer = container.querySelector('.ascii-scroll-container > div') as HTMLElement;
    await waitFor(() => expect(spacer.style.justifyContent).toBe('flex-end'));
    expect(spacer.style.minHeight).not.toBe('');
  });

  it('keeps the positioned layout when the block does not contain the end', async () => {
    const { container, findByText } = render(
      <AsciiViewer totalBytes={10_000} autoScroll={false} />
    );

    expect(await findByText('line 0')).toBeInTheDocument();
    const spacer = container.querySelector('.ascii-scroll-container > div') as HTMLElement;
    expect(spacer.style.justifyContent).toBe('');
    expect(spacer.style.position).toBe('relative');
  });
});
