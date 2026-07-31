// Retries `fn` up to `maxRetries` times on throw, waiting
// `baseDelayMs * (attempt + 1)` ms before each retry (linear backoff).
export async function fetchWithRetry<T>(
  fn: () => Promise<T>,
  maxRetries = 2,
  baseDelayMs = 1000,
): Promise<T> {
  for (let attempt = 0; attempt <= maxRetries; attempt++) {
    try {
      return await fn();
    } catch (err) {
      if (attempt < maxRetries) {
        await new Promise((r) => setTimeout(r, baseDelayMs * (attempt + 1)));
      } else {
        throw err;
      }
    }
  }
  throw new Error("retry exhausted"); // unreachable, for TS
}
