import { useCallback, useEffect, useRef } from 'react';

/**
 * A setTimeout wrapper that automatically clears all pending timeouts
 * when the component unmounts, preventing state updates on unmounted components.
 *
 * Usage:
 *   const safeSetTimeout = useSafeTimeout();
 *   safeSetTimeout(() => setMsg(''), 2000);
 */
export function useSafeTimeout() {
  const timeoutsRef = useRef<Set<ReturnType<typeof setTimeout>>>(new Set());

  useEffect(() => {
    const timeouts = timeoutsRef.current;
    return () => {
      timeouts.forEach(clearTimeout);
      timeouts.clear();
    };
  }, []);

  const safeSetTimeout = useCallback((fn: () => void, ms: number) => {
    const id = setTimeout(() => {
      timeoutsRef.current.delete(id);
      fn();
    }, ms);
    timeoutsRef.current.add(id);
    return id;
  }, []);

  return safeSetTimeout;
}
