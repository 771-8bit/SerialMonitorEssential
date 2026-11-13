import { describe, it, expect, vi, beforeEach } from 'vitest';
import { act, render, waitFor } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import AsciiViewer from '../components/viewers/AsciiViewer';
import { TAIL_START_LINE } from '../components/viewers/scrollUtils';
import { ROW_HEIGHT } from '../components/viewers/viewerConstants';

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

  it.each([false, true])(
    'keeps a complete short capture at the top as partial lines end (wrap=%s)',
    async (lineWrap) => {
      let count = 1;
      let trailingNewline = false;
      invokeMock.mockImplementation(async (cmd) => {
        if (cmd !== 'get_ascii_lines') return undefined;
        return {
          lines: lines(0, count),
          total_lines: count + Number(trailingNewline),
          start_line: 0,
          reaches_end: true,
        };
      });
      const { container, rerender, findByText } = render(
        <AsciiViewer totalBytes={10} autoScroll={true} lineWrap={lineWrap} />
      );
      await findByText('line 0');
      const spacer = container.querySelector('.ascii-scroll-container > div') as HTMLElement;
      // Include the empty index entry after CR/LF, the next partial line, and
      // growth through a typical viewport boundary. None may shift line 0.
      for (const [nextCount, newline] of [
        [1, true],
        [2, false],
        [2, true],
        [25, true],
        [26, false],
      ] as const) {
        count = nextCount;
        trailingNewline = newline;
        rerender(
          <AsciiViewer
            totalBytes={count * 10 + Number(newline)}
            autoScroll={true}
            lineWrap={lineWrap}
          />
        );
        await waitFor(() => expect(container.querySelectorAll('.ascii-row')).toHaveLength(count));
        expect(spacer.style.justifyContent).toBe('flex-start');
        expect(spacer.style.minHeight).toBe(`${count * ROW_HEIGHT}px`);
        expect(spacer.firstElementChild).not.toHaveStyle({ position: 'absolute' });
      }
    }
  );

  it('discards an in-flight reply after Clear and fetches new data even at the previous byte count', async () => {
    let finishOld!: (payload: unknown) => void;
    invokeMock.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finishOld = resolve;
        })
    );
    const { container, rerender, findByText } = render(
      <AsciiViewer totalBytes={10} autoScroll={true} />
    );
    await waitFor(() => expect(finishOld).toBeDefined());
    rerender(<AsciiViewer totalBytes={0} autoScroll={true} />);
    await act(async () => {
      finishOld({
        lines: [{ offset: 0, text: 'old', timestamp: null }],
        total_lines: 1,
        start_line: 0,
        reaches_end: true,
      });
    });
    expect(container.querySelectorAll('.ascii-row')).toHaveLength(0);
    invokeMock.mockResolvedValue({
      lines: [{ offset: 0, text: 'new', timestamp: null }],
      total_lines: 1,
      start_line: 0,
      reaches_end: true,
    });
    rerender(<AsciiViewer totalBytes={10} autoScroll={true} />);
    expect(await findByText('new')).toBeInTheDocument();
    expect(container).not.toHaveTextContent('old');
    const spacer = container.querySelector('.ascii-scroll-container > div') as HTMLElement;
    expect(spacer.style.justifyContent).toBe('flex-start');
  });
});
