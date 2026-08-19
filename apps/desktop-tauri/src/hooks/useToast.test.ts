/**
 * useToast tests — verify that:
 *   1. each variant maps to its own default auto-dismiss duration
 *      (success=2.5s / info=3s / warning=4s / error=5s) so the
 *      stack reads correctly across fast delete / rename bursts;
 *   2. explicit duration overrides the per-variant default;
 *   3. duration=0 disables auto-dismiss (manual close only).
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { toast, __resetToastsForTest } from "./useToast";

describe("useToast duration defaults", () => {
  type Spy = {
    mock: { calls: Array<[unknown, number | undefined, ...unknown[]]> };
    mockRestore: () => void;
  };
  let setTimeoutSpy: Spy;
  // We replace setTimeout so the real auto-dismiss timers never
  // fire during the test (they would dismiss toasts *after* the
  // assertion runs and contaminate the next test).
  beforeEach(() => {
    __resetToastsForTest();
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    setTimeoutSpy = vi
      .spyOn(globalThis, "setTimeout")
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      .mockImplementation((() => 0) as any) as unknown as Spy;
  });

  afterEach(() => {
    setTimeoutSpy.mockRestore();
    __resetToastsForTest();
  });

  it.each([
    ["success", 2500],
    ["info", 3000],
    ["warning", 4000],
    ["error", 5000],
  ] as const)(
    "%s variant schedules %i ms auto-dismiss by default",
    (variant, expected) => {
      toast("test", variant);
      const calledWithDefault = setTimeoutSpy.mock.calls.some(
        ([_handler, ms]) => ms === expected,
      );
      expect(calledWithDefault).toBe(true);
    },
  );

  it("explicit duration overrides the variant default", () => {
    toast("test", "success", 1234);
    expect(setTimeoutSpy).toHaveBeenCalledWith(expect.any(Function), 1234);
  });

  it("duration=0 disables auto-dismiss (manual close only)", () => {
    toast("test", "error", 0);
    // The branch in toast() guards `if (duration > 0) setTimeout(...)`
    // — no setTimeout should have been called with 0 for this push.
    const all = setTimeoutSpy.mock.calls;
    const zeroCalls = all.filter(([, ms]) => ms === 0);
    expect(zeroCalls.length).toBe(0);
  });
});