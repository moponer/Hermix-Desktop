// EnhanceButton —— 增强提示词按钮的行为测试。
//
// 覆盖的是**状态机**（这是最容易回归的部分）：
// 空草稿禁用 → 点击增强 → 回填结果 → 再点还原 → 失败只报错不回填。
// 接线错误（prop 传了没解构、mock 路径写错）只有真渲染才暴露。

import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { useState } from "react";
import { EnhanceButton } from "./EnhanceButton";
import { polishEnhance } from "../api/polish";
import { toastError } from "../hooks/useToast";

vi.mock("../api/polish", () => ({ polishEnhance: vi.fn() }));
vi.mock("../hooks/useToast", () => ({ toastError: vi.fn() }));

const mockEnhance = vi.mocked(polishEnhance);
const mockToast = vi.mocked(toastError);

/** 把组件接成真实回路：onResult 直接回写 draft（模拟 Composer）。 */
function Harness({ initial }: { initial: string }) {
  const [draft, setDraft] = useState(initial);
  return <EnhanceButton draft={draft} onResult={(t) => setDraft(t)} />;
}

beforeEach(() => {
  mockEnhance.mockReset();
  mockToast.mockReset();
});

describe("EnhanceButton", () => {
  it("草稿为空时禁用（没有可增强的内容）", () => {
    render(<Harness initial="   " />);
    expect(screen.getByRole("button", { name: "增强" })).toBeDisabled();
  });

  it("点击后调增强接口，并把结果回填（按钮变成「还原」）", async () => {
    mockEnhance.mockResolvedValue("## 角色\n你是…\n\n## 任务\n1. …");
    render(<Harness initial="帮我写个周报" />);

    fireEvent.click(screen.getByRole("button", { name: "增强" }));

    await waitFor(() => {
      expect(screen.getByRole("button", { name: "还原" })).toBeInTheDocument();
    });
    expect(mockEnhance).toHaveBeenCalledTimes(1);
    expect(mockEnhance.mock.calls[0][0].draft).toBe("帮我写个周报");
  });

  it("再点一次 = 还原原文（不再调接口）", async () => {
    mockEnhance.mockResolvedValue("## 角色\n你是…");
    render(<Harness initial="帮我写个周报" />);

    fireEvent.click(screen.getByRole("button", { name: "增强" }));
    await waitFor(() => {
      expect(screen.getByRole("button", { name: "还原" })).toBeInTheDocument();
    });

    mockEnhance.mockClear();
    fireEvent.click(screen.getByRole("button", { name: "还原" }));

    await waitFor(() => {
      expect(screen.getByRole("button", { name: "增强" })).toBeInTheDocument();
    });
    expect(mockEnhance).not.toHaveBeenCalled();
  });

  it("增强失败 → 只报错，不回填（草稿保持原样）", async () => {
    mockEnhance.mockRejectedValue(new Error("后端不可达"));
    render(<Harness initial="帮我写个周报" />);

    fireEvent.click(screen.getByRole("button", { name: "增强" }));

    await waitFor(() => {
      expect(mockToast).toHaveBeenCalledWith("后端不可达");
    });
    // 失败后仍是「增强」态，说明没有把结果写进去
    expect(screen.getByRole("button", { name: "增强" })).toBeInTheDocument();
  });

  it("textarea 聚焦时 Ctrl+Shift+/ 触发增强", async () => {
    mockEnhance.mockResolvedValue("## 角色\n你是…");
    const ta = document.createElement("textarea");
    document.body.appendChild(ta);
    ta.focus();
    render(<Harness initial="帮我写个周报" />);

    fireEvent.keyDown(ta, { code: "Slash", key: "?", ctrlKey: true, shiftKey: true });

    await waitFor(() => {
      expect(mockEnhance).toHaveBeenCalledTimes(1);
    });
    document.body.removeChild(ta);
  });
});
