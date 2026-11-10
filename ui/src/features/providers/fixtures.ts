import type { ProviderKindInfo, ProviderProfile } from './api';

/** Test data shaped like the backend's answers. */
export const kindInfos: ProviderKindInfo[] = [
  {
    kind: 'openai',
    defaultBaseUrl: 'https://api.openai.com/v1',
    requiresApiKey: true,
    requiresCli: false,
    capabilities: { chat: true, vision: true, transcription: true },
    suggestedModels: { chat: 'gpt-4o-mini', vision: 'gpt-4o', transcription: 'whisper-1' },
  },
  {
    kind: 'anthropic',
    defaultBaseUrl: 'https://api.anthropic.com',
    requiresApiKey: true,
    requiresCli: false,
    capabilities: { chat: true, vision: true, transcription: false },
    suggestedModels: { chat: 'claude-sonnet', vision: 'claude-sonnet', transcription: null },
  },
  {
    kind: 'gemini',
    defaultBaseUrl: 'https://generativelanguage.googleapis.com',
    requiresApiKey: true,
    requiresCli: false,
    capabilities: { chat: true, vision: true, transcription: true },
    suggestedModels: {
      chat: 'gemini-flash',
      vision: 'gemini-flash',
      transcription: 'gemini-flash',
    },
  },
  {
    kind: 'deepseek',
    defaultBaseUrl: 'https://api.deepseek.com',
    requiresApiKey: true,
    requiresCli: false,
    capabilities: { chat: true, vision: false, transcription: false },
    suggestedModels: { chat: 'deepseek-chat', vision: null, transcription: null },
  },
  {
    kind: 'ollama',
    defaultBaseUrl: 'http://localhost:11434',
    requiresApiKey: false,
    requiresCli: false,
    capabilities: { chat: true, vision: false, transcription: false },
    suggestedModels: { chat: 'llama3.1', vision: 'llava', transcription: null },
  },
  {
    kind: 'claude_cli',
    defaultBaseUrl: '',
    requiresApiKey: false,
    requiresCli: true,
    capabilities: { chat: true, vision: true, transcription: false },
    suggestedModels: { chat: 'sonnet', vision: 'sonnet', transcription: null },
  },
  {
    kind: 'codex_cli',
    defaultBaseUrl: '',
    requiresApiKey: false,
    requiresCli: true,
    capabilities: { chat: true, vision: true, transcription: false },
    suggestedModels: { chat: 'gpt-5-codex', vision: 'gpt-5-codex', transcription: null },
  },
];

export const openAiProfile: ProviderProfile = {
  id: 'p-openai',
  kind: 'openai',
  displayName: 'OpenAI work',
  model: 'gpt-4o',
  baseUrl: null,
  secretRef: 's1',
  supportsVision: true,
  capabilities: { chat: true, vision: true, transcription: true },
};

export const anthropicProfile: ProviderProfile = {
  id: 'p-anthropic',
  kind: 'anthropic',
  displayName: 'Claude work',
  model: 'claude-sonnet',
  baseUrl: null,
  secretRef: 's3',
  supportsVision: true,
  capabilities: { chat: true, vision: true, transcription: false },
};

export const deepSeekProfile: ProviderProfile = {
  id: 'p-deepseek',
  kind: 'deepseek',
  displayName: 'DeepSeek cheap',
  model: 'deepseek-chat',
  baseUrl: null,
  secretRef: 's2',
  supportsVision: false,
  capabilities: { chat: true, vision: false, transcription: false },
};

export const ollamaProfile: ProviderProfile = {
  id: 'p-ollama',
  kind: 'ollama',
  displayName: 'Local llama',
  model: 'llama3.1',
  baseUrl: null,
  secretRef: null,
  supportsVision: false,
  capabilities: { chat: true, vision: false, transcription: false },
};

export const claudeCliProfile: ProviderProfile = {
  id: 'p-claude-cli',
  kind: 'claude_cli',
  displayName: 'Claude subscription',
  model: 'sonnet',
  baseUrl: null,
  secretRef: null,
  supportsVision: false,
  capabilities: { chat: true, vision: true, transcription: false },
};

export const codexCliProfile: ProviderProfile = {
  id: 'p-codex-cli',
  kind: 'codex_cli',
  displayName: 'Codex subscription',
  model: 'gpt-5-codex',
  baseUrl: null,
  secretRef: null,
  supportsVision: false,
  capabilities: { chat: true, vision: true, transcription: false },
};

export const openAiSecret = { id: 's1', name: 'OPENAI_KEY', description: '', createdAt: 1 };
export const deepSeekSecret = { id: 's2', name: 'DEEPSEEK_KEY', description: '', createdAt: 2 };
export const secrets = [openAiSecret, deepSeekSecret];
