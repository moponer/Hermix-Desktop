// 一次性：把轮次句柄注册表（TurnOutcome + 中断/steer/轮次记录）搬到 turn_registry.rs
import { readFileSync, writeFileSync } from "node:fs";

const MOD = new URL("./mod.rs", import.meta.url);
const OUT = new URL("./turn_registry.rs", import.meta.url);
const lines = readFileSync(MOD, "utf8").split("\n");

// ── A 块：TurnOutcome + signal_interrupt + take_steer_buffer ──
let aStart = lines.findIndex((l) => l.trim() === "enum TurnOutcome {");
if (aStart < 0) throw new Error("找不到 enum TurnOutcome");
// 向上吞 derive 与文档注释
while (
  aStart > 0 &&
  (lines[aStart - 1].startsWith("#[derive") || lines[aStart - 1].startsWith("///"))
) {
  aStart--;
}
const steerFn = lines.findIndex((l) => l.startsWith("fn take_steer_buffer("));
if (steerFn < 0) throw new Error("找不到 take_steer_buffer");
let aEnd = steerFn;
while (aEnd < lines.length && lines[aEnd] !== "}") aEnd++;

// ── B 块：interrupt_session … interrupt_all ──
let bStart = lines.findIndex((l) => l.trimStart().startsWith("/// Interrupt **one** session's in-flight turn"));
if (bStart < 0) throw new Error("找不到 interrupt_session 文档注释");
const allFn = lines.findIndex((l) => l.trim() === "fn interrupt_all(&self) {");
if (allFn < 0) throw new Error("找不到 interrupt_all");
let bEnd = allFn;
while (bEnd < lines.length && lines[bEnd] !== "    }") bEnd++;

const aBlock = lines.slice(aStart, aEnd + 1);
const bBlock = lines.slice(bStart, bEnd + 1);

// A：模块级 item → 去缩进（TurnOutcome 的 impl 内方法保留 4 空格）+ pub(super)
const aOut = aBlock.map((l) => {
  if (/^enum TurnOutcome/.test(l)) return "pub(super) " + l;
  if (/^fn [a-z_]+\(/.test(l)) return l.replace(/^fn /, "pub(super) fn ");
  if (/^    fn [a-z_]+\(/.test(l)) return l.replace(/^    fn /, "    pub(super) fn ");
  return l;
});

// B：impl 方法保持缩进，只给私有方法补 pub(super)（pub 的保持原样）
const bOut = bBlock.map((l) =>
  /^    fn [a-z_]+\(/.test(l) ? l.replace(/^    fn /, "    pub(super) fn ") : l,
);

const header = `//! 轮次句柄注册表：中断控制器、steer 缓冲、轮次终态记录。
//!
//! 从 \`local_agent_service\` 搬出的第五批（P1-13）。这批方法都只碰两个
//! \`Arc<Mutex<HashMap<..>>>\` 注册表（\`active_interrupts\` / \`steer_buffers\`）
//! 与 \`task_record_sink\`，与 LLM 调用、工具执行无关。
//!
//! 为什么放子模块而不是继续堆在组合根：中断 / steer / 轮次记录是**同一套
//! 会话生命周期**的三面，合并在一处才看得见"登记与销号必须成对"这条约束
//! （\`register_active\` 与 \`unregister_active\` 之间是整轮执行）。
//!
//! 字段仍是父模块私有的 —— 子模块可以访问父模块的私有字段，所以搬家不需要
//! 放宽任何可见性。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::agent_loop::AgentLoop;
use crate::interrupt::InterruptController;

use super::LocalAgentService;

`;

writeFileSync(OUT, header + aOut.join("\n") + "\n\n" + bOut.join("\n") + "\n");

// 从 mod.rs 删两段（先删后面的，避免行号偏移）
const withoutB = [...lines.slice(0, bStart), ...lines.slice(bEnd + 1)];
const trimmedB = withoutB.slice(0, bStart);
while (trimmedB.length && trimmedB[trimmedB.length - 1].trim() === "") trimmedB.pop();
const merged = [
  ...trimmedB,
  "",
  ...withoutB.slice(bStart),
];
const finalLines = [...merged.slice(0, aStart), ...merged.slice(aEnd + 1)];
writeFileSync(MOD, finalLines.join("\n"));

console.log(
  `A: ${aStart + 1}..${aEnd + 1} (${aBlock.length} 行), B: ${bStart + 1}..${bEnd + 1} (${bBlock.length} 行)`,
);
