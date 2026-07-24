/**
 * fetchWithRetry — retry a fetch operation with linear backoff.
 *
 * @param fn The async operation to attempt. Return the result on success,
 *           or throw on failure to trigger a retry.
 * @param maxRetries Maximum number of retries (default 2).
 * @param baseDelayMs Base delay in ms; each retry waits `baseDelayMs * (attempt + 1)`.
 */
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
