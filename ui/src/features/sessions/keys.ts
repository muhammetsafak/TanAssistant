import type { SessionStatusDto } from '../../ipc/bindings';

export interface SessionListQuery {
  status: SessionStatusDto | null;
  offset: number;
  limit: number;
}

/** Query keys of the session library; invalidating `all` refreshes every session view. */
export const sessionKeys = {
  all: ['sessions'] as const,
  list: (query: SessionListQuery) => ['sessions', 'list', query] as const,
  search: (text: string) => ['sessions', 'search', text] as const,
  detail: (sessionId: string) => ['sessions', 'detail', sessionId] as const,
  processing: (sessionId: string) => ['sessions', 'processing', sessionId] as const,
  estimate: (sessionId: string) => ['sessions', 'estimate', sessionId] as const,
  storage: ['storage-usage'] as const,
};
