// useArtifacts — artifact extraction and path verification
// Inputs: activeSession (messages) + workspaceRoot
// Outputs: verified artifact list + selected artifact + path-click handler

import { useState, useEffect, useMemo, useCallback, useRef } from "react";
import { extractArtifacts, type Artifact, type ArtifactKind } from "../utils/artifacts";
import { tryArtifactPathExists } from "../utils/artifactReader";

function classifyFromExtension(ext: string): ArtifactKind {
  const CODE_EXTS = new Set(["ts", "tsx", "js", "jsx", "mjs", "rs", "py", "go", "java", "kt", "c", "cpp", "cxx", "h", "hpp", "rb", "php", "sh", "bash", "css", "html", "vue", "sql", "json", "yaml", "yml", "toml", "xml", "md", "txt", "log", "ini"]);
  const DOC_EXTS = new Set(["docx", "doc", "pdf", "rtf", "odt"]);
  const DATA_EXTS = new Set(["csv", "tsv", "xlsx", "xls", "xlsm", "parquet", "sqlite", "db"]);
  const IMAGE_EXTS = new Set(["png", "jpg", "jpeg", "gif", "webp", "svg", "ico", "bmp"]);
  const OFFICE_EXTS = new Set(["docx", "doc", "xlsx", "xls", "pptx", "ppt", "odt", "ods", "odp"]);
  if (IMAGE_EXTS.has(ext)) return "image";
  if (OFFICE_EXTS.has(ext)) return "office";
  if (CODE_EXTS.has(ext)) return "code";
  if (DATA_EXTS.has(ext)) return "data";
  if (DOC_EXTS.has(ext)) return "doc";
  return "file";
}

interface UseArtifactsInput {
  /** The active session (for message scanning). */
  activeSession: { messages: any[] } | null | undefined;
  /** Workspace root directory for resolving relative paths. */
  workspaceRoot: string | null | undefined;
  /** Callback to open the right sidebar when a file is clicked. */
  onOpenRightSidebar: () => void;
}

interface UseArtifactsOutput {
  /** All extracted artifacts (including unverified ones — for count display). */
  rawArtifacts: Artifact[];
  /** Verified artifacts whose paths exist on disk. */
  artifacts: Artifact[];
  /** Currently selected artifact in the right sidebar. */
  selectedArtifactId: string | null;
  /** Programmatically select an artifact by id. */
  setSelectedArtifactId: (id: string | null) => void;
  /** Click handler for file paths in markdown bodies. */
  handlePathClick: (path: string) => void;
}

export function useArtifacts({
  activeSession,
  workspaceRoot,
  onOpenRightSidebar,
}: UseArtifactsInput): UseArtifactsOutput {
  // synchronous extraction from message text
  const rawArtifacts = useMemo(() => {
    if (!activeSession) return [];
    return extractArtifacts(activeSession.messages, workspaceRoot);
  }, [activeSession, workspaceRoot]);

  // async existence check via Tauri IPC
  const [verifiedArtifacts, setVerifiedArtifacts] = useState<Artifact[]>([]);
  useEffect(() => {
    const raw = rawArtifacts;
    if (raw.length === 0) {
      // Don't wipe verifiedArtifacts when rawArtifacts is
      // empty — synthetic artifacts added by handlePathClick (e.g. a
      // user clicks a path link the LLM mentioned in prose that the
      // regex extractor missed) would be immediately cleared, causing
      // the preview modal to flash open then close.  Instead, keep
      // only synthetic (non-message-extracted) artifacts.
      setVerifiedArtifacts((prev) => prev.filter((a) => a.tool === "inline"));
      return;
    }
    let cancelled = false;
    Promise.all(
      raw.map(async (a) => {
        const exists = await tryArtifactPathExists(a.path, workspaceRoot);
        return { a, exists };
      }),
    ).then((results) => {
      if (cancelled) return;
      const next = results.filter((r) => r.exists).map((r) => r.a);
      // Preserve synthetic artifacts added by
      // handlePathClick. Without this, the async verification
      // effect would overwrite verifiedArtifacts and silently
      // drop artifacts the user just clicked — causing the
      // preview modal to flash open then immediately close.
      // We keep any artifact whose path is NOT in rawArtifacts
      // (i.e. it was injected, not extracted from messages).
      const rawPaths = new Set(raw.map((a) => a.path.replace(/\\/g, "/")));
      setVerifiedArtifacts((prev) => {
        const synthetics = prev.filter((a) => !rawPaths.has(a.path.replace(/\\/g, "/")));
        return [...next, ...synthetics];
      });
      if (typeof window !== "undefined" && window.localStorage?.getItem("hermixDebugArtifacts") === "1") {
        const dropped = results.filter((r) => !r.exists).map((r) => r.a.path);
        console.debug(
          "[artifacts] raw=%d verified=%d workspaceRoot=%s",
          raw.length, next.length, workspaceRoot ?? "(null)",
        );
        if (dropped.length > 0) {
          console.debug("[artifacts] dropped paths:", dropped);
        }
      }
    });
    return () => { cancelled = true; };
  }, [rawArtifacts, workspaceRoot]);

  const artifacts = verifiedArtifacts;

  // Keep a ref to the latest artifacts so
  // handlePathClick doesn't need artifacts in its dependency
  // array. This avoids recreating the callback (and busting
  // MemoMessageBubble's memo) every time the verification
  // effect updates verifiedArtifacts.
  const artifactsRef = useRef(artifacts);
  artifactsRef.current = artifacts;

  const [selectedArtifactId, setSelectedArtifactId] = useState<string | null>(null);

  /** Programmatic file open from inside a markdown body. */
  const handlePathClick = useCallback(
    (path: string) => {
      const existing = artifactsRef.current.find((a) => a.path === path || a.path.endsWith(path));
      if (existing) {
        setSelectedArtifactId(existing.id);
      } else {
        const ext = path.split(".").pop()?.toLowerCase() ?? "";
        const synth: Artifact = {
          id: `path:${path}`,
          name: path.split(/[\\/]/).pop() ?? path,
          path,
          kind: classifyFromExtension(ext),
          tool: "inline",
          timestamp: new Date().toISOString(),
          isAbsolute: /^[A-Za-z]:[\\/]/.test(path) || path.startsWith("/"),
        };
        setVerifiedArtifacts((prev) => {
          // De-dup: don't add if an artifact with the same
          // normalized path already exists.
          const norm = synth.path.replace(/\\/g, "/");
          if (prev.some((a) => a.path.replace(/\\/g, "/") === norm)) return prev;
          return [...prev, synth];
        });
        setSelectedArtifactId(synth.id);
      }
      onOpenRightSidebar();
    },
    [onOpenRightSidebar],
  );

  return { rawArtifacts, artifacts, selectedArtifactId, setSelectedArtifactId, handlePathClick };
}
