// src/api/index.ts
//
// Barrel re-export — the front-end's transport-layer entry point.
//
// Every component reaches the backend through `import * as api from "../api"`,
// which now resolves to this file. The actual implementations live in:
//
//   - ./rest  : REST-style request/response (`apiInvoke` → stdio bridge's
//               `http` channel, which itself dispatches into the server's
//               existing axum router — same handler code, different transport).
//   - ./ws    : streaming upstream/downstream (channel names + a `listen`-based
//               subscribe helper that filters the `stdio-frame` event).
//   - ./types : pure TS types re-exported here so callers can keep
//               `import type { ... } from "../api"`.
//
// Why a barrel: before this migration, `import * as api from "../api"` had no
// resolver — TypeScript silently fell back to "first .ts file alphabetically"
// (or just errored in strict mode). Re-stating the entry point explicitly
// removes that ambiguity and keeps the call sites unchanged across the TCP →
// stdio transport switch.
export * from "./rest";
export * from "./ws";
export * from "./types";
