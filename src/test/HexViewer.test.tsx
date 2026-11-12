import { act, render, waitFor } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import HexViewer from '../components/viewers/HexViewer';

class MockResizeObserver {
  observe() {}
  unobserve() {}
  disconnect() {}
}
globalThis.ResizeObserver = MockResizeObserver as unknown as typeof ResizeObserver;

describe('HexViewer capture reset', () => {
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
