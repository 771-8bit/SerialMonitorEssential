import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, waitFor, fireEvent } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import SettingsPanel, { SerialConfig } from '../components/SettingsPanel';

// invoke / listen は src/test/setup.ts でグローバルにモックされている
const invokeMock = vi.mocked(invoke);

interface BridgeStatusInfo {
  enabled: boolean;
  port: number;
  connections: number;
  last_activity: { kind: string; bytes: number; at_ms: number; preview: string } | null;
}

const bridgeStatus = (over: Partial<BridgeStatusInfo> = {}): BridgeStatusInfo => ({
  enabled: true,
  port: 57320,
  connections: 0,
  last_activity: null,
  ...over,
});

const defaultConfig: SerialConfig = {
  baud_rate: 115200,
  data_bits: 8,
  flow_control: 'None',
  parity: 'None',
  stop_bits: 1,
  dtr: true,
  rts: true,
};

function renderPanel() {
  return render(
    <SettingsPanel
      ports={['COM1']}
      selectedPort="COM1"
      onPortChange={vi.fn()}
      onRefreshPorts={vi.fn()}
      config={defaultConfig}
      onConfigChange={vi.fn()}
      isConnected={false}
      onConnect={vi.fn()}
      onDisconnect={vi.fn()}
    />
  );
}

const bridgeCheckbox = () => screen.getByLabelText('AI Bridge');

describe('SettingsPanel - AI Bridge', () => {
  beforeEach(() => {
    invokeMock.mockReset();
    invokeMock.mockResolvedValue(bridgeStatus());
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it('renders the bridge toggle off by default and hides the endpoint', () => {
    renderPanel();

    const checkbox = bridgeCheckbox();
    expect(checkbox).toBeInTheDocument();
    expect(checkbox).not.toBeChecked();
    // 既定 OFF なのでエンドポイントは出さない
    expect(screen.queryByText('127.0.0.1:57320')).not.toBeInTheDocument();
    // 起動時に勝手にブリッジを開始しない
    expect(invokeMock).not.toHaveBeenCalledWith('bridge_set', expect.anything());
  });

  it('has the localhost-only tooltip', () => {
    renderPanel();
    expect(bridgeCheckbox().closest('label')).toHaveAttribute(
      'title',
      'ローカルのAIエージェント用ブリッジ（127.0.0.1のみ・既定OFF）'
    );
  });

  it('calls bridge_set and shows the endpoint when enabled', async () => {
    renderPanel();

    fireEvent.click(bridgeCheckbox());

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith('bridge_set', { enabled: true, port: null });
    });
    expect(await screen.findByText('127.0.0.1:57320')).toBeInTheDocument();
    expect(bridgeCheckbox()).toBeChecked();
  });

  it('calls bridge_set with enabled=false when toggled back off', async () => {
    renderPanel();

    fireEvent.click(bridgeCheckbox());
    await screen.findByText('127.0.0.1:57320');

    invokeMock.mockResolvedValue(bridgeStatus({ enabled: false }));
    fireEvent.click(bridgeCheckbox());

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith('bridge_set', { enabled: false, port: null });
    });
    await waitFor(() => {
      expect(screen.queryByText('127.0.0.1:57320')).not.toBeInTheDocument();
    });
  });

  it('reverts the toggle when the backend rejects the start', async () => {
    const consoleError = vi.spyOn(console, 'error').mockImplementation(() => {});
    invokeMock.mockRejectedValue('Failed to bind 127.0.0.1:57320');

    renderPanel();
    fireEvent.click(bridgeCheckbox());

    await waitFor(() => {
      expect(bridgeCheckbox()).not.toBeChecked();
    });
    expect(screen.queryByText('127.0.0.1:57320')).not.toBeInTheDocument();
    consoleError.mockRestore();
  });

  it('shows the port reported by the backend, not the hardcoded default', async () => {
    invokeMock.mockResolvedValue(bridgeStatus({ port: 60000 }));

    renderPanel();
    fireEvent.click(bridgeCheckbox());

    expect(await screen.findByText('127.0.0.1:60000')).toBeInTheDocument();
  });

  it('renders connection count, last send activity, and the content preview', async () => {
    invokeMock.mockResolvedValue(
      bridgeStatus({
        connections: 2,
        last_activity: {
          kind: 'send',
          bytes: 12,
          at_ms: Date.UTC(2026, 0, 1, 12, 0, 0),
          preview: 'AT+GMR\r\n',
        },
      })
    );

    renderPanel();
    fireEvent.click(bridgeCheckbox());

    const hint = await screen.findByText(/接続 2/);
    expect(hint).toHaveTextContent('送信 12 bytes');
    // 人間が AI の送信内容を確認できる（制御文字は可視化される）
    expect(hint).toHaveTextContent('AT+GMR··');
  });

  it('opens the AI guide window from the Setup Guide button', async () => {
    invokeMock.mockResolvedValue(undefined);
    renderPanel();

    fireEvent.click(screen.getByRole('button', { name: 'Setup Guide' }));

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith('open_ai_guide_window');
    });
  });
});
