import { describe, it, expect } from 'vitest';
import { buildSearchUrl, SEARCH_PAGE_SIZE } from './searchUrl';

describe('buildSearchUrl', () => {
  it('encodes query and includes limit+offset', () => {
    expect(buildSearchUrl('a b/c', 0)).toBe(
      `/api/v1/search/a%20b%2Fc?limit=${SEARCH_PAGE_SIZE}&offset=0`,
    );
  });
  it('uses the given offset', () => {
    expect(buildSearchUrl('cat', 40)).toBe(
      `/api/v1/search/cat?limit=${SEARCH_PAGE_SIZE}&offset=40`,
    );
  });
});
