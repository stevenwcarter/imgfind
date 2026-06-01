import { describe, it, expect } from 'vitest';
import { selectViewState } from './searchViewState';

describe('selectViewState', () => {
  it('idle before any search', () => {
    expect(
      selectViewState({ hasSearched: false, loading: false, error: null, resultCount: 0 }),
    ).toBe('idle');
  });
  it('loading wins', () => {
    expect(selectViewState({ hasSearched: true, loading: true, error: null, resultCount: 5 })).toBe(
      'loading',
    );
  });
  it('error after load', () => {
    expect(
      selectViewState({ hasSearched: true, loading: false, error: 'boom', resultCount: 0 }),
    ).toBe('error');
  });
  it('empty when searched with no results', () => {
    expect(
      selectViewState({ hasSearched: true, loading: false, error: null, resultCount: 0 }),
    ).toBe('empty');
  });
  it('results when matches exist', () => {
    expect(
      selectViewState({ hasSearched: true, loading: false, error: null, resultCount: 3 }),
    ).toBe('results');
  });
});
