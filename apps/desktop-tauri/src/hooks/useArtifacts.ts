// useArtifacts — artifact extraction and path verification
// Inputs: activeSession (messages) + workspaceRoot
// Outputs: verified artifact list + selected artifact + path-click handler

import { useState, useEffect, useMemo, useCallback, useRef } from "react";
import { extractArtifacts, canonicalPath, type Artifact, type ArtifactKind } from "../utils/artifacts";
import { tryArtifactPathExists } from "../utils/artifactReader";
import { toastError } from "./useToast";

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
  /** 点中一个文件（对话正文里的路径）→ 由外层开一个预览标签。 */
  onOpenArtifact: (artifact: Artifact) => void;
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
  onOpenArtifact,
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
      const rawPaths = new Set(raw.map((a) => canonicalPath(a.path)));
      setVerifiedArtifacts((prev) => {
        const synthetics = prev.filter((a) => !rawPaths.has(canonicalPath(a.path)));
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
    async (path: string) => {
      // 匹配口径与 App 侧开 tab 完全一致（canonicalPath）：同一文件写成
      // `F:\a\b.md` 或 `F:/a/b.md` 必须命中同一条，否则会合成一个多余
      // 产物，进而在侧栏开出两个重复标签。
      const target = canonicalPath(path);
      const existing = artifactsRef.current.find(
        (a) => canonicalPath(a.path) === target || a.path.endsWith(path),
      );
      if (existing) {
        // verifiedArtifacts 里的每一条都过了盘上存在性检查，直接开。
        onOpenArtifact(existing);
        return;
      }
      // 正文里提到、但提取器没抓到的路径：先探一次盘再合成产物。
      // 原来是无条件合成 + 直接开预览，文件已经删掉时点开就是一个打不开
      // 的空白标签，看上去像"点了没反应" —— 明确报错比假装能开省事。
      if (!(await tryArtifactPathExists(path, workspaceRoot))) {
        toastError(
          /[\\/]/.test(path)
            ? `文件不存在或不在工作区内：${path}`
            : `只有文件名、没有目录，定位不到：${path}`,
        );
        return;
      }
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
        // canonical path already exists.
        if (prev.some((a) => canonicalPath(a.path) === target)) return prev;
        return [...prev, synth];
      });
      onOpenArtifact(synth);
    },
    [onOpenArtifact, workspaceRoot],
  );

  return { rawArtifacts, artifacts, selectedArtifactId, setSelectedArtifactId, handlePathClick };
}
