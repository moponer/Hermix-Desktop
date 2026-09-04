// src/lib/workspaceFileInterceptor.ts
//
// Translates `stdio://workspace-file/<workspaceId>/<filename>` URLs
// (the sentinel returned by `workspaceFileUrl` in `api/rest.ts`)
// into a real `blob:` URL by round-tripping the bridge for the
// attachment bytes.
//
// Why a global interceptor instead of a per-cell React component?
// `TableView.tsx` renders `<img src={workspaceFileUrl(...)} />` in a
// sync cell renderer and is wired into TanStack Table's column
// defs — adding a wrapper component would force every cell to
// re-render twice (mount → fetch → setState). A `MutationObserver`
// on `document.body` rewrites the `src` as soon as the DOM node is
// inserted, which is roughly equivalent in latency but keeps every
// call site unchanged.
//
// We dedupe on URL so multiple <img> with the same src share one
// in-flight bridge round-trip; once resolved, the blob URL is
// memoised for the page lifetime.

import { fetchJSONBinary } from "../api";

const STORAGE_KEY_PREFIX = "stdio://workspace-file/";

interface CacheEntry {
  blobUrl: string;
  pending: Promise<string> | null;
}

const cache = new Map<string, CacheEntry>();

/** Parse a `stdio://workspace-file/<workspaceId>/<filename>` URL
 *  into its two path components. Returns `null` for any other
 *  scheme — the observer must leave non-target URLs alone. */
function parseTarget(url: string): { workspaceId: string; filename: string } | null {
  if (!url.startsWith(STORAGE_KEY_PREFIX)) return null;
  const rest = url.slice(STORAGE_KEY_PREFIX.length);
  const slash = rest.indexOf("/");
  if (slash <= 0) return null;
  return {
    workspaceId: decodeURIComponent(rest.slice(0, slash)),
    filename: decodeURIComponent(rest.slice(slash + 1)),
  };
}

/** Resolve the sentinel URL to a `blob:` URL. Dedupes concurrent
 *  requests for the same URL and caches the resolved blob URL so
 *  subsequent <img> mounts hit the in-memory cache instead of
 *  hitting the bridge. */
async function resolveSentinelUrl(url: string): Promise<string> {
  const cached = cache.get(url);
  if (cached) {
    if (cached.blobUrl) return cached.blobUrl;
    if (cached.pending) return cached.pending;
  }

  const target = parseTarget(url);
  if (!target) {
    throw new Error(`workspaceFileUrl: unparseable sentinel: ${url}`);
  }

  const entry: CacheEntry = { blobUrl: "", pending: null };
  cache.set(url, entry);

  entry.pending = (async () => {
    // Match the server route in `crates/hermix-server/src/dashboard/workspaces.rs`
    // — the backend expects GET /api/workspaces/:id/files/:filename with the
    // workspace id and filename URL-encoded.
    const path =
      `/api/workspaces/${encodeURIComponent(target.workspaceId)}` +
      `/files/${encodeURIComponent(target.filename)}`;
    const blob = await fetchJSONBinary("GET", path);
    const blobUrl = URL.createObjectURL(blob);
    entry.blobUrl = blobUrl;
    entry.pending = null;
    return blobUrl;
  })();

  try {
    return await entry.pending;
  } catch (e) {
    // Failure path: evict the failed entry so a later render can
    // retry (the user may have just installed the workspace dir).
    cache.delete(url);
    throw e;
  }
}

/** Apply the resolved blob URL to an <img>. Skips nodes whose src
 *  has already moved on (the observer races with React's own
 *  diffing, so the same node can fire twice in quick succession). */
function applyToImage(img: HTMLImageElement): void {
  const target = parseTarget(img.src);
  if (!target) return;

  // Kick off the resolution but don't await — the observer fires
  // synchronously per node and we want to drain the whole batch.
  resolveSentinelUrl(img.src)
    .then((blobUrl) => {
      if (img.src !== blobUrl) {
        img.src = blobUrl;
      }
    })
    .catch((e) => {
      // The <img> shows the browser's broken-image icon for failed
      // loads; the console error gives us the diagnostic. We don't
      // want to spam retries on every render, so the cache above is
      // only populated on success — failures fall through and the
      // next render will try again.
      console.warn("[workspaceFileInterceptor] resolve failed:", e);
    });
}

/** Install the document-level observer. Idempotent — calling twice
 *  is a no-op (the second call returns the same handle). The
 *  caller (typically `main.tsx`) calls this once at boot. */
let installed: MutationObserver | null = null;

export function installWorkspaceFileInterceptor(): void {
  if (installed) return;

  const observer = new MutationObserver((mutations) => {
    for (const mutation of mutations) {
      mutation.addedNodes.forEach((node) => {
        if (!(node instanceof HTMLImageElement)) return;
        applyToImage(node);
      });
      // Also rewrite when an existing <img> gets a new src (React
      // diffing can change the attribute without re-creating the
      // node — happens when the same row scrolls into view with a
      // different attachment).
      if (
        mutation.type === "attributes" &&
        mutation.target instanceof HTMLImageElement &&
        mutation.attributeName === "src"
      ) {
        applyToImage(mutation.target);
      }
    }
  });

  observer.observe(document.body, {
    childList: true,
    subtree: true,
    attributes: true,
    attributeFilter: ["src"],
  });
  installed = observer;

  // Drain anything already on the page (the observer's first
  // callback only fires for NEW nodes; we have to walk the tree
  // once at install time to catch the splash / eager-rendered
  // images).
  document.body.querySelectorAll("img").forEach((img) => applyToImage(img));
}
