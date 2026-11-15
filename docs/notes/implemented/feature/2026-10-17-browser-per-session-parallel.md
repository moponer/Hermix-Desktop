# Decision Note: 浏览器会话键控多路并行——每对话一个 Edge，小窗多标签

Status: implemented — 2026-10-17

## Decision

1. **浏览器会话按 `_session_id` 键控**（`browser/mod.rs`）：`static SESSION` 单例改为
   `SESSIONS: Mutex<HashMap<String, Session>>`。键来自 registry 的
   `inject_session_id`（`dispatch_with_context` 已端到端注入，测试钉死）；无上下文
   （测试 / headless 调度）回退 `"default"`。每个键独立 Edge 实例（独立一次性
   profile、独立 screencast 通道），对话间不再互踩页面。
2. **资源闸**：并发实例上限 `MAX_SESSIONS=4`（超限报错并提示先 `browser_close`）；
   空闲 ≥15min 的实例在下一次 `ensure` 时回收（Drop 杀 Edge 树 + 清 profile，
   内核级无孤儿保证不变）。
3. **帧流多路**：screencast 键控化（`CHANNELS`/`PAGES` map）；sidecar 转发器
   多路轮询所有活动键，payload 增加 `session` 字段（单帧 ~100KB，转发节流
   50ms/200ms 两档）；broadcast Lagged 丢帧保新，监控流宁可跳帧不可积压。
4. **小窗多标签**（`BrowserView.tsx`）：帧按会话缓存，>1 会话时顶部渲染标签；
   默认跟随最新活动会话，点标签钉住；切换即从缓存绘帧（无需等下一帧）。
   暂停/恢复按当前显示的会话键下发（`/api/browser-view/paused` 带可选
   `session`，缺省 `default`）。
5. **自动弹窗按键去重**（`stdio_bridge::note_browser_frame`）：burst 检测从
   单计时器改为 per-session 计时表——任一会话开始新帧流（≥60s 空窗）都弹
   小窗；burst 内不重复。

## Alternatives considered

- **进程级所有权锁**（同一时刻只允许一个会话持有浏览器，其余报"被占用"）—
  否决：实现最省，但把"并行"降级成"排队"，用户明确要并行。
- **每键互斥锁**（`HashMap<String, Mutex<Session>>`）— 否决：Drop 时需要整表
  锁清理（Job Object/profile），分键锁反而引入跨锁竞态；单表锁 + 单次调用
  串行化已满足"click/snapshot 不交错"，跨对话序列隔离由键保证。
- **小窗自动跟随切换 + 禁止钉住** — 否决：两路并行时画面来回跳，观察者
  失去稳定的参照系；钉住 + 显式切换成本极低。

## Consequences

- 获得：多对话并行操作浏览器互不干扰；小窗可对照观察两路画面。
- 代价：最多 4 个 Edge 实例（数百 MB 级/个）——上限 + 空闲回收兜底；
  单表锁使所有会话的浏览器调用全局串行（监控场景可接受，非吞吐路径）。
- 风险：LLM 不知道键的存在（`_session_id` 是注入的，不占 schema）——无感知、
  无需提示词；`browser_close` 只关本对话，跨对话清理依赖空闲回收。
