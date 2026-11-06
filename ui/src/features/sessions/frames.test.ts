import { describe, expect, it, vi } from 'vitest';

import { frameImageUrl } from './api';
import { suggestedFileName } from './format';
import { nearestFrame } from './frames';
import { frames } from './fixtures';

vi.mock('@tauri-apps/api/core', () => ({
  convertFileSrc: (path: string) => `asset://localhost/${encodeURIComponent(path)}`,
}));

describe('nearestFrame', () => {
  it('picks the frame closest in time, before or after', () => {
    expect(nearestFrame(frames, 0)?.id).toBe('f1');
    expect(nearestFrame(frames, 40_000)?.id).toBe('f2');
    expect(nearestFrame(frames, 70_000)?.id).toBe('f2');
    expect(nearestFrame(frames, 250_000)?.id).toBe('f3');
  });

  it('is null without frames', () => {
    expect(nearestFrame([], 1000)).toBeNull();
  });
});

describe('frameImageUrl', () => {
  it('builds the asset URL of a frame inside the session folder', () => {
    expect(frameImageUrl(String.raw`C:\data\sessions\s1`, 'frames/f1.png')).toBe(
      `asset://localhost/${encodeURIComponent(String.raw`C:\data\sessions\s1\frames\f1.png`)}`,
    );
    expect(frameImageUrl('/data/sessions/s1/', 'frames/f1.png')).toBe(
      `asset://localhost/${encodeURIComponent('/data/sessions/s1/frames/f1.png')}`,
    );
  });

  it('refuses paths that could leave the session folder', () => {
    expect(frameImageUrl('/data/s1', '../other/f.png')).toBeNull();
    expect(frameImageUrl('/data/s1', 'frames/../../f.png')).toBeNull();
    expect(frameImageUrl('/data/s1', '/etc/passwd')).toBeNull();
    expect(frameImageUrl('/data/s1', String.raw`C:\Windows\x.png`)).toBeNull();
    expect(frameImageUrl('/data/s1', '')).toBeNull();
  });
});

describe('suggestedFileName', () => {
  it('keeps the title and drops characters file systems refuse', () => {
    expect(suggestedFileName('Weekly sync')).toBe('Weekly sync.md');
    expect(suggestedFileName('Q3: plan/review?')).toBe('Q3 plan review.md');
    expect(suggestedFileName('  ...  ')).toBe('notes.md');
  });
});
