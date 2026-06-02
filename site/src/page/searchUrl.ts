export const SEARCH_PAGE_SIZE = 40;

export interface SearchResultItem {
  path: string;
  distance: number;
  fileSize: number | null;
}

export interface SearchResponse {
  results: SearchResultItem[];
  hasMore: boolean;
}

export function buildSearchUrl(query: string, offset: number): string {
  return `/api/v1/search/${encodeURIComponent(query)}?limit=${SEARCH_PAGE_SIZE}&offset=${offset}`;
}
