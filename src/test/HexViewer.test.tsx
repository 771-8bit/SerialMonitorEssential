import { act, fireEvent, render, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import HexViewer from '../components/viewers/HexViewer';
import { ROW_HEIGHT } from '../components/viewers/viewerConstants';

class MockResizeObserver {
  observe() {}
  unobserve() {}
  disconnect() {}
}
globalThis.ResizeObserver = MockResizeObserver as unknown as typeof ResizeObserver;

describe('HexViewer capture reset', () => {
  afterEach(() => vi.restoreAllMocks());

  it.each([
    [1000, 209],
    [2_000_000, 211],
  ])(
    'keeps the last row inside the spacer with %i rows and a %ipx viewport',
    async (initialTotal, viewportHeight) => {
      vi.spyOn(HTMLElement.prototype, 'clientHeight', 'get').mockReturnValue(viewportHeight);
      // Model real overflow too: an absolutely positioned block can enlarge
      // scrollHeight even when its parent's explicit height is unchanged.
      vi.spyOn(HTMLElement.prototype, 'scrollHeight', 'get').mockImplementation(function (
        this: HTMLElement
      ) {
        const spacer = this.firstElementChild as HTMLElement | null;
        const block = spacer?.firstElementChild as HTMLElement | null;
        const height = Number.parseFloat(spacer?.style.height || '0');
        const blockBottom =
          Number.parseFloat(block?.style.top || '0') + (block?.children.length || 0) * ROW_HEIGHT;
        return Math.max(height, blockBottom);
      });
      const invokeMock = vi.mocked(invoke);
      invokeMock.mockReset();
      let total = initialTotal;
      invokeMock.mockImplementation(async (cmd, args) => {
        if (cmd !== 'get_display_rows') return undefined;
        const a = args as { startRow: number; rowCount: number };
        return {
          rows: Array.from({ length: Math.min(a.rowCount, total - a.startRow) }, (_, i) => ({
            offset: (a.startRow + i) * 16,
            hex: `row ${a.startRow + i}`,
            ascii: 'x',
            timestamp: null,
          })),
          total_rows: total,
        };
      });
      const { container, rerender, findByText } = render(
        <HexViewer totalBytes={total * 16} autoScroll={true} />
      );
      await findByText(`row ${total - 1}`);
      const viewport = container.querySelector('.hex-scroll-container') as HTMLElement;
      const spacer = viewport.firstElementChild as HTMLElement;
      expect(viewport.scrollHeight).toBe(Number.parseFloat(spacer.style.height));
      expect(viewport.scrollTop).toBe(viewport.scrollHeight - viewportHeight);
      fireEvent.scroll(viewport, { target: { scrollTop: viewport.scrollHeight - viewportHeight } });
      expect(viewport.scrollHeight).toBe(Number.parseFloat(spacer.style.height));
      total += 100;
      rerender(<HexViewer totalBytes={total * 16} autoScroll={true} />);
      await findByText(`row ${total - 1}`);
      expect(viewport.scrollHeight).toBe(Number.parseFloat(spacer.style.height));
      expect(viewport.scrollTop).toBe(viewport.scrollHeight - viewportHeight);
    }
  );

  it('keeps a delayed upward jump visible and preserves the first row during new input', async () => {
    vi.spyOn(HTMLElement.prototype, 'clientHeight', 'get').mockReturnValue(400);
    vi.spyOn(HTMLElement.prototype, 'scrollHeight', 'get').mockImplementation(function (
      this: HTMLElement
    ) {
      return Number.parseFloat((this.firstElementChild as HTMLElement | null)?.style.height || '0');
    });
    const invokeMock = vi.mocked(invoke);
    invokeMock.mockReset();
    let total = 2_000_000;
    let deferHead = false;
    let finishHead: ((payload: unknown) => void) | undefined;
    const payload = (start: number, count: number) => ({
      rows: Array.from({ length: Math.min(count, total - start) }, (_, i) => ({
        offset: (start + i) * 16,
        hex: `row ${start + i}`,
        ascii: 'x',
        timestamp: null,
      })),
      total_rows: total,
    });
    invokeMock.mockImplementation(async (cmd, args) => {
      if (cmd !== 'get_display_rows') return undefined;
      const a = args as { startRow: number; rowCount: number };
      if (a.startRow === 0 && deferHead) {
        deferHead = false;
        return new Promise((resolve) => {
          finishHead = resolve;
        });
      }
      return payload(a.startRow, a.rowCount);
    });
    const { container, rerender, findByText } = render(
      <HexViewer totalBytes={total * 16} autoScroll={true} />
    );
    await findByText(`row ${total - 1}`);
    const viewport = container.querySelector('.hex-scroll-container') as HTMLElement;
    deferHead = true;
    fireEvent.scroll(viewport, { target: { scrollTop: 0 } });
    await waitFor(() => expect(finishHead).toBeDefined());
    const rows = viewport.firstElementChild!.firstElementChild as HTMLElement;
    expect(rows.style.top).toBe('0px');
    await act(async () => {
      finishHead!(payload(0, 120));
    });
    await findByText('row 0');
    total += 100;
    rerender(<HexViewer totalBytes={total * 16} autoScroll={true} />);
    await waitFor(() =>
      expect(invokeMock).toHaveBeenLastCalledWith(
        'get_display_rows',
        expect.objectContaining({ startRow: 0 })
      )
    );
    expect(viewport.scrollTop).toBe(0);
    expect(rows.style.top).toBe('0px');
    expect(await findByText('row 0')).toBeInTheDocument();
  });

  it('rejects a pre-Clear reply and fetches new bytes at the same offset and count', async () => {
    const invokeMock = vi.mocked(invoke);
    invokeMock.mockReset();
    let finishOld!: (payload: unknown) => void;
    invokeMock.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finishOld = resolve;
        })
    );
    const { container, rerender, findByText } = render(
      <HexViewer totalBytes={3} autoScroll={false} />
    );
    await waitFor(() => expect(finishOld).toBeDefined());
    rerender(<HexViewer totalBytes={0} autoScroll={false} />);
    await act(async () => {
      finishOld({
        rows: [{ offset: 0, hex: '6F 6C 64', ascii: 'old', timestamp: null }],
        total_rows: 1,
      });
    });
    expect(container).not.toHaveTextContent('old');
    invokeMock.mockResolvedValue({
      rows: [{ offset: 0, hex: '6E 65 77', ascii: 'new', timestamp: null }],
      total_rows: 1,
    });
    rerender(<HexViewer totalBytes={3} autoScroll={false} />);
    expect(await findByText('6E 65 77')).toBeInTheDocument();
    expect(container).toHaveTextContent('new');
    expect(container).not.toHaveTextContent('old');
  });
});
