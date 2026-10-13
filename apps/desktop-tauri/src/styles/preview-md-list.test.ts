/**
 * Regression test for preview-pane list markers.
 *
 * Tailwind 4 preflight ships with `list-style: none` on `ul`/`ol`,
 * which strips the bullet/number markers from markdown lists. The
 * chat-side `.prose` rule in components-prose.css re-applies them
 * (`list-style-type: disc` / `decimal`); the preview pane has to do
 * the same re-apply step, so .md files render with invisible bullets
 * (see screenshot of agent-tool-calling.md showing list items but no
 * `-` markers).
 *
 * These rules moved from `code-view.css` to `preview-doc.css` when the
 * document-preview typography was split out of the code view; the
 * guarantee follows the rule, so this check reads the new file.
 *
 * A file-content check (instead of a DOM computed-style check) sidesteps
 * jsdom not loading Tailwind preflight and not running layout, while
 * still catching the exact regression the user reported.
 */
import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

const cssPath = resolve(__dirname, "preview-doc.css");
const css = readFileSync(cssPath, "utf-8");

describe(".preview-pane .preview-md-body list styles", () => {
  it("declares list-style-type: disc on ul so unordered lists render with bullets", () => {
    expect(css).toMatch(
      /\.preview-pane\s+\.preview-md-body\s+ul\s*\{[^}]*list-style-type:\s*disc/,
    );
  });

  it("declares list-style-type: decimal on ol so ordered lists render with numbers", () => {
    expect(css).toMatch(
      /\.preview-pane\s+\.preview-md-body\s+ol\s*\{[^}]*list-style-type:\s*decimal/,
    );
  });

  it("sets list-style-position: outside on li so markers sit outside the text box", () => {
    expect(css).toMatch(
      /\.preview-pane\s+\.preview-md-body\s+li\s*\{[^}]*list-style-position:\s*outside/,
    );
  });

  it("tints li::marker muted so list markers read like the .prose chat convention", () => {
    expect(css).toMatch(
      /\.preview-pane\s+\.preview-md-body\s+li::marker\s*\{[^}]*color:/,
    );
  });
});
