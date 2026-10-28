/**
 * 欢迎页「夜航」面板 —— 空态入口（A · 夜航方向，2026-10-27 定）。
 *
 * 三件事：
 *   1. 锚点是**一句承诺**，不是产品名字标。HERMIX 退到角落做署名。
 *   2. 右侧一张吉祥物插画 —— 高级感有一半来自它，纯排版撑不出。
 *   3. 能力入口是**可读的场景句**而不是同款灰药丸：点之前就知道
 *      点了会发生什么（原来的药丸点了会塞一段隐藏 prompt）。
 *
 * 暗色是**局部**的：所有颜色走 welcome-night.css 里的 --night-* 变量，
 * 与全局主题解耦 —— 浅色主题下这块面板依然是暗的。
 * 原因：用户要的是「入口页的气质」，不是全 app 换皮。
 */
import { Sparkles, AppWindow, Workflow, FileText } from "lucide-react";

/** 「今天可以做的」= 一句能直接当 prompt 用的完整诉求。
 *  写成完整句子而不是名词（"独立应用"），是为了兑现"点之前就知道
 *  点了会得到什么"这个功能契约 —— 旧版的灰药丸点了会塞一段隐藏
 *   prompt，用户点之前完全不知道会发生什么。 */
interface NightTask {
  key: string;
  /** 卡片上显示的一句场景 */
  label: string;
  /** 点击后填进输入框的完整诉求 */
  prompt: string;
  icon: typeof AppWindow;
}

const NIGHT_TASKS: NightTask[] = [
  {
    key: "app",
    label: "把「快递台账统计」做成能双击打开的小应用",
    prompt: "帮我做一个能双击打开的桌面小应用，用来统计快递台账：",
    icon: AppWindow,
  },
  {
    key: "flow",
    label: "每天下班自动汇总报价单并生成纪要",
    prompt: "帮我搭一个自动化工作流，触发条件和步骤是：每天下班自动汇总报价单并生成纪要",
    icon: Workflow,
  },
  {
    key: "pdf",
    label: "把 PDF 里的表格直接导出成 Excel",
    prompt: "把这份 PDF 里的表格和数据提取出来，导出成 Excel",
    icon: FileText,
  },
];

interface WelcomeNightProps {
  /** 点场景 → 填入输入框 */
  onPickScene: (prompt: string) => void;
}

export function WelcomeNight({ onPickScene }: WelcomeNightProps) {
  return (
    <div className="night-panel w-full">
      <div className="night-mark">HERMIX</div>
      <h1 className="night-title">
        把想法变成<em>能跑的东西</em>
      </h1>
      <p className="night-sub">
        说一句话，它就动手：桌面应用、自动化工作流、整理好的文档。
      </p>

      <div className="night-card">
        <div className="night-card-text">
          <div className="night-card-label">
            <Sparkles aria-hidden="true" />
            今天可以做的
          </div>
          <ul className="night-card-list">
            {NIGHT_TASKS.map((t) => (
              <li key={t.key}>
                <button
                  type="button"
                  className="night-task"
                  onClick={() => onPickScene(t.prompt)}
                  title={t.prompt}
                >
                  <t.icon aria-hidden="true" />
                  <span>{t.label}</span>
                </button>
              </li>
            ))}
          </ul>
        </div>
        <img
          className="night-mascot"
          src="mascot-512.png"
          alt="Hermix 吉祥物"
        />
      </div>
    </div>
  );
}
