import { useCallback, useEffect, useRef, useState } from "react";

/** Run an async loader and expose { data, loading, error, reload }. */
export function useAsync<T>(fn: () => Promise<T>, deps: unknown[] = []) {
  const [data, setData] = useState<T | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [tick, setTick] = useState(0);

  useEffect(() => {
    let alive = true;
    setLoading(true);
    setError(null);
    Promise.resolve()
      .then(fn)
      .then((result) => {
        if (!alive) return;
        setData(result);
        setLoading(false);
      })
      .catch((reason) => {
        if (!alive) return;
        setError(reason instanceof Error ? reason.message : "加载失败");
        setLoading(false);
      });
    return () => {
      alive = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [tick, ...deps]);

  const reload = useCallback(() => setTick((t) => t + 1), []);
  return { data, loading, error, reload };
}

/** Poll a loader every `interval` ms; also reloads immediately on mount. */
export function usePolling<T>(fn: () => Promise<T>, interval = 15_000, deps: unknown[] = []) {
  const { data, loading, error, reload } = useAsync(fn, deps);
  useEffect(() => {
    const id = window.setInterval(() => reload(), interval);
    return () => window.clearInterval(id);
  }, [reload, interval]);
  return { data, loading, error, reload };
}

/** A ticker that re-renders every `interval` ms (for relative timestamps). */
export function useTicker(interval = 30_000) {
  const [, setTick] = useState(0);
  useEffect(() => {
    const id = window.setInterval(() => setTick((t) => t + 1), interval);
    return () => window.clearInterval(id);
  }, [interval]);
}

/** Stable identity for a function prop, so `usePolling` won't loop. */
export function useStable<T extends () => unknown>(fn: T): T {
  const ref = useRef<T>(fn);
  ref.current = fn;
  return useCallback(() => ref.current(), []) as T;
}
