// MemoryTab：设置页「记忆管理」的数据流锚点。
// echarts 不进 jsdom —— MemoryGraph 以桩件替换（把节点渲染成按钮，
// 点击走同一个 onSelectNode 回调），这里验证的是页面级数据流：
// 图谱加载与统计、节点详情弹窗、视图切换、角色面板。
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { MemoryTab } from "./MemoryTab";
import * as api from "../../api";

vi.mock("../../api", () => ({
  getMemoryGraph: vi.fn(),
  getMemoryItem: vi.fn(),
  deleteMemoryItem: vi.fn(),
  searchMemory: vi.fn(),
  getMemoryOverview: vi.fn(),
  pinMemory: vi.fn(),
  getDailyMemoryList: vi.fn(),
  listPersonalities: vi.fn(),
  getPersonality: vi.fn(),
  savePersonality: vi.fn(),
  removePersonality: vi.fn(),
  getPersonaSoul: vi.fn(),
  savePersonaSoul: vi.fn(),
  getPersonaMemory: vi.fn(),
  savePersonaMemory: vi.fn(),
  getPersonaUser: vi.fn(),
  savePersonaUser: vi.fn(),
}));

// MemoryGraph 桩件：echarts 依赖 canvas，jsdom 里跑不起来。
// 桩件保留 props 合同（nodes/edges/onSelectNode）。
vi.mock("./MemoryGraph", () => ({
  MemoryGraph: ({ nodes, onSelectNode }: { nodes: Array<{ id: string; label: string }>; onSelectNode: (n: { id: string }) => void }) => (
    <div data-testid="memory-graph-stub">
      {nodes.map((n) => (
        <button key={n.id} onClick={() => onSelectNode(n as never)}>
          {n.label}
        </button>
      ))}
    </div>
  ),
  KIND_LABELS: {
    fact: "事实",
    daily: "每日记忆",
    md: "文本条目",
    reflection: "反思",
    turn: "对话",
  },
}));

const GRAPH: api.MemoryGraphResponse = {
  nodes: [
    { id: "fact:abc", kind: "fact", label: "user.name:张三", preview: "张三", salience: 0.8, created_at: "2026-10-07", pinned: false },
    { id: "daily:2026-10-07", kind: "daily", label: "2026-10-07", preview: "- 12:00 [s1] 总结", salience: 0.5, created_at: "2026-10-07", pinned: false },
    { id: "md:k1@0", kind: "md", label: "全局条目", preview: "全局条目", salience: 0.7, created_at: "2026-10-07", pinned: false },
  ],
  edges: [],
  stats: {
    frames: 12, facts: 3, reflections: 2, daily_days: 5, md_entries: 1, db_bytes: 2048,
    per_day: [["2026-10-07", 4]],
  },
  truncated: false,
};

beforeEach(() => {
  vi.mocked(api.getMemoryGraph).mockResolvedValue(GRAPH);
  vi.mocked(api.getMemoryItem).mockResolvedValue({
    id: "fact:abc", kind: "fact", content: '{"key": "user.name:张三"}', created_at: "2026-10-07", salience: 0.8,
  });
  vi.mocked(api.getDailyMemoryList).mockResolvedValue([
    { date: "2026-10-07", goals: "g", progress: "p", conclusions: "- 12:00 [s1] 总结", updated_at: 1 },
  ]);
  vi.mocked(api.listPersonalities).mockResolvedValue([
    { name: "coder", summary: "编程", size: 10, updated_at: null, is_builtin: true },
    { name: "my-role", summary: "自定义", size: 10, updated_at: 1, is_builtin: false },
  ]);
  vi.mocked(api.getPersonaSoul).mockResolvedValue({ path: "soul", content: "# soul", updated_at: null });
  vi.mocked(api.getPersonality).mockResolvedValue({ path: "<builtin:coder>", content: "# coder prompt", updated_at: null });
  vi.mocked(api.getPersonaMemory).mockResolvedValue({ path: "MEMORY.md", content: "- 已知内容", updated_at: 1 });
  vi.mocked(api.getPersonaUser).mockResolvedValue({ path: "USER.md", content: "称呼：张三", updated_at: 1 });
});

describe("MemoryTab", () => {
  it("图谱视图：加载统计与节点，点击节点打开详情弹窗", async () => {
    render(<MemoryTab />);
    await waitFor(() => expect(screen.getByText("记忆总览")).toBeInTheDocument());
    expect(screen.getByText("user.name:张三")).toBeInTheDocument();
    fireEvent.click(screen.getByText("user.name:张三"));
    await waitFor(() => expect(api.getMemoryItem).toHaveBeenCalledWith("fact", "fact:abc"));
    expect(await screen.findByText(/"key": "user.name:张三"/)).toBeInTheDocument();
  });

  it("切换到角色视图：列出角色并全文展示内置提示词", async () => {
    render(<MemoryTab />);
    fireEvent.click(screen.getByRole("tab", { name: "角色" }));
    // 内置角色默认选中第一项，提示词全文直接展示（只读）。
    const textarea = await screen.findByRole("textbox");
    expect(textarea).toHaveValue("# coder prompt");
    expect(textarea).toHaveAttribute("readonly");
    expect(await screen.findAllByText("coder").then((els) => els.length)).toBeGreaterThan(0);
    expect(api.getPersonality).toHaveBeenCalledWith("coder");
  });

  it("切换到每日记忆视图：逐日内容展示", async () => {
    render(<MemoryTab />);
    fireEvent.click(screen.getByText("每日记忆"));
    await waitFor(() => expect(api.getDailyMemoryList).toHaveBeenCalled());
    expect(await screen.findByText(/- 12:00 \[s1\] 总结/)).toBeInTheDocument();
  });
});
