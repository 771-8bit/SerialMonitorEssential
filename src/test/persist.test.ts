import { describe, it, expect } from 'vitest';
import { loadSetting, saveSetting, isBoolean, isOneOf } from '../utils/persist';
import { isSerialConfig } from '../utils/serialConfig';

describe('persisted settings (issue #7)', () => {
  it('round-trips a value under the sme. prefix', () => {
    saveSetting('x', 42);
    expect(localStorage.getItem('sme.x')).toBe('42');
    expect(loadSetting('x', 0, (v): v is number => typeof v === 'number')).toBe(42);
  });

  it('falls back to the default for missing, unparsable or invalid values', () => {
    expect(loadSetting('missing', true, isBoolean)).toBe(true);
    localStorage.setItem('sme.broken', '{not json');
    expect(loadSetting('broken', true, isBoolean)).toBe(true);
    localStorage.setItem('sme.mode', '"Bogus"');
    expect(loadSetting('mode', 'Peak', isOneOf(['Peak', 'Average']))).toBe('Peak');
  });

  it('reads the pre-existing autoReconnect entry ("true"/"false")', () => {
    localStorage.setItem('sme.autoReconnect', 'true');
    expect(loadSetting('autoReconnect', false, isBoolean)).toBe(true);
  });

  it('accepts only a complete, in-range serial configuration', () => {
    const ok = {
      baud_rate: 921600,
      data_bits: 7,
      flow_control: 'Hardware',
      parity: 'Even',
      stop_bits: 2,
      dtr: false,
      rts: true,
    };
    expect(isSerialConfig(ok)).toBe(true);
    expect(isSerialConfig({ ...ok, baud_rate: 0 })).toBe(false);
    expect(isSerialConfig({ ...ok, baud_rate: 1.5 })).toBe(false);
    expect(isSerialConfig({ ...ok, parity: 'Mark' })).toBe(false);
    expect(isSerialConfig({ ...ok, dtr: undefined })).toBe(false);
    expect(isSerialConfig(null)).toBe(false);
  });
});
