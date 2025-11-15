import { useEffect } from 'react';

export const useKeyPress = (targetKey: string, handler: () => void, condition: boolean = true) => {
  useEffect(() => {
    if (!condition) return;

    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === targetKey) {
        handler();
      }
    };

    document.addEventListener('keydown', handleKeyDown);
    return () => document.removeEventListener('keydown', handleKeyDown);
  }, [targetKey, handler, condition]);
};
