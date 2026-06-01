export type SearchViewState = 'idle' | 'loading' | 'error' | 'empty' | 'results';

export function selectViewState(args: {
  hasSearched: boolean;
  loading: boolean;
  error: string | null;
  resultCount: number;
}): SearchViewState {
  if (args.loading) return 'loading';
  if (args.error) return 'error';
  if (!args.hasSearched) return 'idle';
  if (args.resultCount === 0) return 'empty';
  return 'results';
}
