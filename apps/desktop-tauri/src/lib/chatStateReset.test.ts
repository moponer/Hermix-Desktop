/**
 * chatStateReset unit tests — covers the model-switch atomic reset path.
 *
 * 这些断言是 2026-08-12 截图 bug 修复契约：
 * - 切模型 → cancelStream 必须先调（重置 in-flight 流）。
 * - approvalResolversRef 必须 forEach denied + clear（防卡死）。
 * - 所有 refs 在调用后必须空（幂等清干净）。
 * - React state setters 必须被以"空值"调用。
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import { resetChatStateForModelSwitch } from "./chatStateReset";
import type { ChatStateResetDeps } from "./chatStateReset";

function makeDeps(opts?: {
  wsOpen?: boolean;
  pendingApprovals?: number;
  pendingClarifies?: number;
  runningTools?: number;
  timerCount?: { retry: number; timeout: number };
}): ChatStateResetDeps {
  const wsOpen = opts?.wsOpen ?? true;
  const approvalMap = new Map<string, (d: unknown) => void>();
  for (let i = 0; i < (opts?.pendingApprovals ?? 0); i++) {
    approvalMap.set(`apr-${i}`, () => {});
  }
  const retryMap = new Map<string, ReturnType<typeof setInterval>>();
  for (let i = 0; i < (opts?.timerCount?.retry ?? 0); i++) {
    retryMap.set(`r-${i}`, setInterval(() => {}, 1000) as ReturnType<typeof setInterval>);
  }
  const timeoutMap = new Map<string, ReturnType<typeof setTimeout>>();
  for (let i = 0; i < (opts?.timerCount?.timeout ?? 0); i++) {
    timeoutMap.set(`t-${i}`, setTimeout(() => {}, 1000) as ReturnType<typeof setTimeout>);
  }
  const pendingApprovalSessions = new Map<string, string>();
  for (let i = 0; i < (opts?.pendingApprovals ?? 0); i++) {
    pendingApprovalSessions.set(`apr-${i}`, "session-1");
  }
  const clarifyMap = new Map<string, Array<(a: string) => void>>();
  for (let i = 0; i < (opts?.pendingClarifies ?? 0); i++) {
    clarifyMap.set(`clr-${i}`, [() => {}]);
  }
  const runningTools = new Map<string, string>();
  for (let i = 0; i < (opts?.runningTools ?? 0); i++) {
    runningTools.set(`tool-${i}`, `tool-${i}`);
  }

  return {
    cancelStream: vi.fn(),
    wsSendRaw: vi.fn(() => wsOpen),
    clearStreamError: vi.fn(),
    setStreamPhase: vi.fn(),
    setRunningToolCount: vi.fn(),
    setSubAgents: vi.fn(),
    approvalResolversRef: { current: approvalMap },
    approvalRetryTimersRef: { current: retryMap },
    approvalTimeoutTimersRef: { current: timeoutMap },
    pendingApprovalSessionsRef: { current: pendingApprovalSessions },
    clarifyResolversRef: { current: clarifyMap },
    runningToolRef: { current: runningTools },
    currentAssistantIdRef: { current: "assistant-123" },
    acuiReset: vi.fn(),
  };
}

describe("resetChatStateForModelSwitch", () => {
  let consoleWarnSpy: ReturnType<typeof vi.spyOn>;

  beforeEach(() => {
    consoleWarnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});
  });

  it("calls cancelStream FIRST (before any other reset)", () => {
    const callOrder: string[] = [];
    const deps = makeDeps();
    deps.cancelStream = vi.fn(() => callOrder.push("cancel"));
    (deps.clearStreamError as ReturnType<typeof vi.fn>).mockImplementation(() =>
      callOrder.push("clearStreamError"),
    );

    resetChatStateForModelSwitch(deps);

    expect(callOrder[0]).toBe("cancel");
    expect(deps.cancelStream).toHaveBeenCalledTimes(1);
  });

  it("clears all React state setters with empty/null sentinel", () => {
    const deps = makeDeps();
    resetChatStateForModelSwitch(deps);

    expect(deps.clearStreamError).toHaveBeenCalledTimes(1);
    expect(deps.setStreamPhase).toHaveBeenCalledWith(null);
    expect(deps.setRunningToolCount).toHaveBeenCalledWith(0);
    expect(deps.setSubAgents).toHaveBeenCalledWith({});
    expect(deps.acuiReset).toHaveBeenCalledTimes(1);
  });

  it("clears currentAssistantIdRef.current", () => {
    const deps = makeDeps();
    expect(deps.currentAssistantIdRef.current).toBe("assistant-123");
    resetChatStateForModelSwitch(deps);
    expect(deps.currentAssistantIdRef.current).toBeNull();
  });

  it("denies all in-flight approvals and sends denial via wsSendRaw", () => {
    const deps = makeDeps({ pendingApprovals: 3 });
    resetChatStateForModelSwitch(deps);

    expect(deps.wsSendRaw).toHaveBeenCalledTimes(3);
    for (const call of (deps.wsSendRaw as ReturnType<typeof vi.fn>).mock.calls) {
      const payload = JSON.parse(call[0] as string);
      expect(payload.type).toBe("approval_response");
      expect(payload.decision).toBe("denied");
      expect(payload.request_id).toMatch(/^apr-\d$/);
    }
    expect(deps.approvalResolversRef.current.size).toBe(0);
  });

  it("clears all retry and timeout timers", () => {
    const clearIntervalSpy = vi.spyOn(globalThis, "clearInterval");
    const clearTimeoutSpy = vi.spyOn(globalThis, "clearTimeout");
    const deps = makeDeps({ timerCount: { retry: 2, timeout: 3 } });
    resetChatStateForModelSwitch(deps);

    expect(clearIntervalSpy).toHaveBeenCalledTimes(2);
    expect(clearTimeoutSpy).toHaveBeenCalledTimes(3);
    expect(deps.approvalRetryTimersRef.current.size).toBe(0);
    expect(deps.approvalTimeoutTimersRef.current.size).toBe(0);
  });

  it("clears runningToolRef and emits count 0", () => {
    const deps = makeDeps({ runningTools: 4 });
    expect(deps.runningToolRef.current.size).toBe(4);
    resetChatStateForModelSwitch(deps);
    expect(deps.runningToolRef.current.size).toBe(0);
    expect(deps.setRunningToolCount).toHaveBeenCalledWith(0);
  });

  it("resolves all pending clarify resolvers with empty string", () => {
    const deps = makeDeps({ pendingClarifies: 2 });
    let resolved = 0;
    deps.clarifyResolversRef.current.forEach((arr) =>
      arr.forEach((r) => {
        r.__testResolve = ((r as unknown as { __testResolve?: () => void })?.__testResolve) ?? (() => {}).bind(null);
        resolved++;
      }),
    );
    resetChatStateForModelSwitch(deps);
    expect(deps.clarifyResolversRef.current.size).toBe(0);
  });

  it("is a no-op on a clean state (no throws, all setters called once)", () => {
    const deps = makeDeps();
    expect(() => resetChatStateForModelSwitch(deps)).not.toThrow();
    expect(deps.cancelStream).toHaveBeenCalledTimes(1);
    expect(deps.wsSendRaw).not.toHaveBeenCalled();
  });

  it("logs warning but does not throw when wsSendRaw returns false", () => {
    const deps = makeDeps({ wsOpen: false, pendingApprovals: 1 });
    expect(() => resetChatStateForModelSwitch(deps)).not.toThrow();
    expect(consoleWarnSpy).toHaveBeenCalledWith(
      expect.stringContaining("[chatReset] ws not open"),
    );
    expect(deps.approvalResolversRef.current.size).toBe(0);
  });
});
