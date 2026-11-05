import type { SerialConfig } from '../components/SettingsPanel';

/** Validates a stored serial configuration before it is used (issue #7) */
export function isSerialConfig(v: unknown): v is SerialConfig {
  if (typeof v !== 'object' || v === null) return false;
  const c = v as Record<string, unknown>;
  return (
    typeof c.baud_rate === 'number' &&
    Number.isInteger(c.baud_rate) &&
    c.baud_rate >= 1 &&
    c.baud_rate <= 12_000_000 &&
    [5, 6, 7, 8].includes(c.data_bits as number) &&
    ['None', 'Software', 'Hardware'].includes(c.flow_control as string) &&
    ['None', 'Odd', 'Even'].includes(c.parity as string) &&
    [1, 2].includes(c.stop_bits as number) &&
    typeof c.dtr === 'boolean' &&
    typeof c.rts === 'boolean'
  );
}
