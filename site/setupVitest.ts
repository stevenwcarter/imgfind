import createFetchMock from 'vitest-fetch-mock';
import { vi } from 'vitest';

const fetchMocker = createFetchMock(vi);

fetchMocker.enableMocks();
beforeEach(() => {
  fetchMocker.mockIf(/\/api\/v1\/search.*/, (_req) => {
    const mockBody = {
      limit: 10,
      offset: 0,
      total: 20,
      data: [{ a: 'a' }, { a: 'b' }],
    };

    return {
      body: JSON.stringify(mockBody),
    };
  });
});
