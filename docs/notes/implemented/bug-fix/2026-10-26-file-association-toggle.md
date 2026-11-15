# Decision Note: 文件关联开关「勾了不生效」—— UserChoice 压过了我们写的默认值

Status: implemented — 2026-10-26 交付：勾选时在写默认值的同一步清掉 Explorer 的 UserChoice 记录并广播 SHChangeNotify；列表按「真正生效」给状态，被压住/路径失效都如实显示

## Decision

Windows 判断「这个扩展名用谁打开」有三层，我们此前只处理了第 2 层：

1. `HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.ext\UserChoice`
   —— 用户在资源管理器里点过「打开方式」之后由 Explorer 写入（带哈希）。
   **它存在时压过下面两层**，我们写什么都不生效。
2. `HKCU\Software\Classes\.ext` 的默认值 —— 本程序写的就是这里。
3. `HKLM\Software\Classes\.ext` 的默认值。

本机实测（2026-10-26）：17 个可勾选扩展名里 **12 个**带 UserChoice
（txt / log / yaml / csv / png / jpg / jpeg / gif / webp / svg / bmp / pdf），
只有 md / markdown / json / yml / toml 没有。原 `file_assoc_set` 只写第 2 层
→ 值写进去了，双击打开的还是老程序 → 用户看到的就是「开关无效」。

修法（`apps/desktop-tauri/src-tauri/src/commands.rs`）：

- **勾选** = 写默认值 + **删掉该扩展名的 UserChoice** + `SHChangeNotify(SHCNE_ASSOCCHANGED)`。
  三步缺一不可：少删一步就是「写了但被压住」，少广播一步 Explorer 继续用旧缓存。
- **取消** = 恢复 Backup 子键里的旧默认值 + 顺手清掉指向我们的 UserChoice + 广播。
- `ensure_viewer_progid` 每次勾选都重写 open command → 应用搬目录后自愈
  （否则是「开关显示已接管、双击报找不到应用程序」）。
- 列表不再只报「默认值是不是我们」，而是报**真正生效**：
  被 UserChoice 压住 → `blocked`；viewer 路径失效 → `stale`；否则 `owned`。
  前端据此显示状态文案，开关的选中态 = `owned`（真正生效），不是「我们写进去了」。

UserChoice 能不能删：本机实测当前用户对 `...\FileExts\.txt\UserChoice` 有 DELETE
权限（RegOpenKeyEx 带 DELETE 成功）。Windows 11 对该键做了额外保护（需夺所有权），
所以删除失败时**如实报错并给出人工路径**，不假装成功。

## Alternatives considered

- **写 HKCR（HKLM）而不是 HKCU**：需要管理员权限，且属于静默抢用户关联 ——
  决策笔记 2026-10-09 已明确「用户显式授权才登记」，不采纳。
- **用 `IApplicationAssociationRegistration::SetUserDefaultPrograms`**：官方 API，
  但要求应用在 HKLM `RegisteredApplications` 登记（要管理员），且 Windows 会走
  「默认程序」向导，与「设置页一个开关就完事」的交互不符，不采纳。
- **只改 UI 文案，让用户自己去资源管理器设「打开方式」**：等于把「开关无效」转嫁给
  用户；用户要的正是点一下就生效，不采纳。
- **改写 UserChoice 的 ProgId + Hash 而不删键**：Hash 是 Explorer 的校验位，改了即失效；
  写权限也未必有。直接删更直接。

## Consequences

- 勾选会**删除** Explorer 记下的那条「打开方式」历史（该扩展名）。这是让勾选生效的
  必要代价 —— 那条记录本身就是压住默认值的东西。取消勾选时我们恢复旧默认值
  （Backup 子键），但不重建那条 UserChoice：重建需要 Explorer 侧的有效哈希，我们造不出。
- 勾选后 Explorer 立刻收到广播，不需要重启资源管理器或注销。
- 前端状态文案从两种变成四种（已接管 / 被系统记录压住 / 路径失效 / 其他程序处理），
  后两种都要让用户看懂「为什么不生效、怎么救」。
- `file_assoc_list` 的 JSON 多了 `blocked` / `stale` 字段；只有本仓库自己在消费，
  无外部契约。
- 取消勾选时的旧默认值来自 Backup 子键 —— 只有「本程序勾选过」才留有备份，
  其它程序此前的关联由 Windows 自己的记录（UserChoice）承担，删除后由系统默认接管。
