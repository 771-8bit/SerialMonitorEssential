/// <reference types="vitest/globals" />
import '@testing-library/jest-dom/vitest';

// Mock Tauri API for frontend tests
vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(() => Promise.resolve(() => {})),
}));

vi.mock('@tauri-apps/plugin-dialog', () => ({
  save: vi.fn(),
  ask: vi.fn(),
}));
