import { describe, it, expect, beforeEach } from 'vitest';
import {
  classifyState,
  getStateColor,
  resetStateColors,
} from '../components/plotter/stateTimelinePlugin';

// State colors carry meaning and never collide (docs/27 PF-4). The old
// string-hash hue gave E01/E02/E03 hues one degree apart and painted
// FAULT_OVERTEMP_LATCHED the same green as ON.

const REDS = ['#dc2626', '#f97316', '#be123c', '#ea580c', '#ef4444', '#c2410c'];

describe('state colors', () => {
  beforeEach(() => resetStateColors());

  it('classifies state names by meaning', () => {
    expect(classifyState('FAULT_OVERTEMP_LATCHED')).toBe('abnormal');
    expect(classifyState('error')).toBe('abnormal');
    expect(classifyState('ALARM_2')).toBe('abnormal');
    expect(classifyState('WARNING')).toBe('warning');
    expect(classifyState('RUNNING')).toBe('normal');
    expect(classifyState('on')).toBe('normal');
    expect(classifyState('STOPPED')).toBe('neutral');
    expect(classifyState('IDLE')).toBe('neutral');
    expect(classifyState('HEATING')).toBe('other');
  });

  it('never paints an abnormal state like a normal one', () => {
    const fault = getStateColor('FAULT_OVERTEMP_LATCHED');
    expect(REDS).toContain(fault);
    expect(fault).not.toBe(getStateColor('ON'));
    expect(fault).not.toBe(getStateColor('RUNNING'));
  });

  it('keeps a normal stop apart from an error', () => {
    expect(getStateColor('STOPPED')).not.toBe(getStateColor('ERROR'));
    expect(REDS).not.toContain(getStateColor('STOPPED'));
  });

  it('gives numbered states distinct colors', () => {
    const colors = ['S0', 'S1', 'S2', 'S3', 'BUSY', 'DONE', 'MOTOR_ON', 'MOTOR_OFF'].map((s) =>
      getStateColor(s)
    );
    expect(new Set(colors).size).toBe(colors.length);
  });

  it('gives numbered error codes distinct red colors', () => {
    const colors = ['ERR_01', 'ERR_02', 'ERR_03'].map((s) => getStateColor(s));
    expect(new Set(colors).size).toBe(3);
    colors.forEach((c) => expect(REDS).toContain(c));
  });

  it('keeps a color for the whole session', () => {
    const first = getStateColor('HEATING');
    getStateColor('COOLING');
    expect(getStateColor('HEATING')).toBe(first);
  });

  it('honours custom colors', () => {
    expect(getStateColor('HEATING', { HEATING: '#123456' })).toBe('#123456');
  });
});
