import type { UpdateInfo } from './api';

export const upToDate: UpdateInfo = {
  available: false,
  currentVersion: '1.2.3',
  version: null,
  notes: null,
  pubDate: null,
  canSelfUpdate: true,
  releaseUrl: 'https://example.test/releases',
};

export const newVersion: UpdateInfo = {
  ...upToDate,
  available: true,
  version: '1.4.0',
};

export const settings = {
  uiLanguage: 'tr' as const,
  defaultNoteLanguage: 'tr',
  defaultSpeechLanguage: 'auto',
  frameIntervalMs: 3000,
  frameChangeThreshold: 8,
  autoProcessAfterRecording: true,
  audioRetentionDays: null,
  consentNoticeAcknowledged: true,
  autoCheckUpdates: true,
};
