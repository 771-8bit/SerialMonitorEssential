import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import AiGuideWindow from '../components/aiguide/AiGuideWindow';

// invoke は src/test/setup.ts でグローバルにモックされている
const invokeMock = vi.mocked(invoke);

const guideInfo = (over: Partial<Record<string, unknown>> = {}) => ({
  exe_path: 'C:\\Apps\\serial-monitor-essential.exe',
  bridge_enabled: false,
  bridge_port: 57320,
  app_version: '0.1.0',
  ...over,
});

describe('AiGuideWindow', () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  it('fetches bridge_guide_info and renders the registration command with the real exe path', async () => {
    invokeMock.mockResolvedValue(guideInfo());
    render(<AiGuideWindow />);

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith('bridge_guide_info');
    });
    // 実際の exe パス入りのコピペ用コマンドが表示される
    await waitFor(() => {
      expect(
        screen.getByText(
          'claude mcp add serial-monitor -- "C:\\Apps\\serial-monitor-essential.exe" --mcp'
        )
      ).toBeInTheDocument();
    });
    // JSON 設定例にも同じパスが埋まる（JSON はバックスラッシュをエスケープ）
    expect(
      screen.getByText(/"command": "C:\\\\Apps\\\\serial-monitor-essential\.exe"/)
    ).toBeInTheDocument();
  });

  it('shows OFF status when the bridge is disabled', async () => {
    invokeMock.mockResolvedValue(guideInfo({ bridge_enabled: false }));
    render(<AiGuideWindow />);

    await waitFor(() => {
      expect(screen.getByTestId('bridge-status')).toHaveTextContent('AI Bridge: OFF');
    });
  });

  it('shows ON status with the configured port', async () => {
    invokeMock.mockResolvedValue(guideInfo({ bridge_enabled: true, bridge_port: 60000 }));
    render(<AiGuideWindow />);

    await waitFor(() => {
      expect(screen.getByTestId('bridge-status')).toHaveTextContent(
        'AI Bridge: ON (127.0.0.1:60000)'
      );
    });
  });

  it('lists all seven MCP tools', async () => {
    invokeMock.mockResolvedValue(guideInfo());
    render(<AiGuideWindow />);

    for (const tool of [
      'serial_status',
      'serial_ports',
      'serial_read_tail',
      'serial_read_range',
      'serial_send',
      'serial_send_hex',
      'serial_wait_for',
    ]) {
      expect(screen.getByText(tool)).toBeInTheDocument();
    }
  });

  it('keeps rendering with a placeholder path when the backend call fails', async () => {
    invokeMock.mockRejectedValue(new Error('no backend'));
    render(<AiGuideWindow />);

    // クラッシュせず、プレースホルダのコマンドを表示する
    await waitFor(() => {
      expect(screen.getByText(/claude mcp add serial-monitor/)).toBeInTheDocument();
    });
    expect(screen.getByTestId('bridge-status')).toHaveTextContent('AI Bridge: OFF');
  });
});
