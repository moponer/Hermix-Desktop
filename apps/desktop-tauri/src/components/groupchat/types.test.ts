/**
 * Unit test for withAlpha 鈥?the helper that converts an `hsl(...)`
 * base colour (returned by agentTone.ring) into `hsla(..., A)` so we
 * can apply opacity at the call site.
 *
 * Why this regression test exists: the previous call sites wrote
 * `${tone.ring}33` (intended `#RRGGBBAA`-style hex alpha suffix,
 * 0x33 = 20% opacity). That pattern is valid for hex colours but
 * NOT for `hsl(...)`; the CSS parser treats the trailing `33` as a
 * stray length and silently drops the entire `box-shadow` declaration.
 * Every groupchat bubble ended up with a fully-opaque border, visible
 * as a thick horizontal line below the message text.
 */
import { describe, it, expect } from "vitest";
import { withAlpha, agentTone } from "./types";

describe("withAlpha", () => {
  it("converts `hsl(H, S%, L%)` to `hsla(H, S%, L%, A)` so callers can apply opacity", () => {
    expect(withAlpha("hsl(28, 55%, 55%)", 0.2)).toBe("hsla(28, 55%, 55%, 0.2)");
  });

  it("round-trips through the same alpha for any value 0..1", () => {
    expect(withAlpha("hsl(195, 70%, 50%)", 0)).toBe("hsla(195, 70%, 50%, 0)");
    expect(withAlpha("hsl(195, 70%, 50%)", 1)).toBe("hsla(195, 70%, 50%, 1)");
    expect(withAlpha("hsl(195, 70%, 50%)", 0.33)).toBe("hsla(195, 70%, 50%, 0.33)");
  });

  it("matches what agentTone.ring produces (the only caller in the codebase)", () => {
    // agentTone.ring is `hsl(${h}, 55%, 55%)`; if its shape ever drifts,
    // withAlpha would silently fall through and the bug would return.
    const ring = agentTone("__leader__").ring;
    expect(ring.startsWith("hsl(") && ring.endsWith(")")).toBe(true);
    expect(withAlpha(ring, 0.2)).toBe(
      `hsla(${ring.slice(4, -1)}, 0.2)`,
    );
  });

  it("passes through non-hsl colours unchanged so we can call it safely on any CSS colour", () => {
    expect(withAlpha("#ff0000", 0.2)).toBe("#ff0000");
    expect(withAlpha("rgb(255, 0, 0)", 0.2)).toBe("rgb(255, 0, 0)");
    expect(withAlpha("red", 0.2)).toBe("red");
    expect(withAlpha("", 0.2)).toBe("");
  });
});