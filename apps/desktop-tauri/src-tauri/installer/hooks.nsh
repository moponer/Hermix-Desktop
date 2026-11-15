; hooks.nsh — Tauri NSIS 安装器钩子
; 四个宏会被 Tauri 模板无条件 !insertmacro，全部必须定义（缺一个编译即失败）。
; 校验用宽容匹配：sidecar 落盘文件名是否剥三元组后缀取决于 bundler 行为，
; 两种名字任一存在即算通过，避免把"名字对不上"误报成"文件被杀"。

; ── 收掉仍占着安装目录的进程 ──────────────────────────────────────
;
; 症状（2026-10-29 现场）：升级安装弹
;   「无法打开要写入的文件：D:\…\Hermix Desktop\hermix-viewer.exe」
; 只能中止 / 重试 / 忽略。根因不是权限，是**文件被运行中的进程锁着**：
; 查看器是独立进程，主程序退出后按设计继续运行
; （apps/desktop-tauri/src-tauri/src/shutdown.rs 明确不杀它），于是 NSIS 的
; `File` 覆盖 hermix-viewer.exe 时就写不进去。
;
; 模板自带的 CheckIfAppIsRunning 只检查 ${MAINBINARYNAME}.exe 一个名字，
; 看不到 externalBin 里那批伴生 exe —— 所以必须在这里补齐。
;
; 顺序：**先关主程序，再关伴生进程**。主程序的 sidecar 挂在 Job Object 上
; （kill-on-close），随主进程被终止而一棵树死；反过来先杀 sidecar 会留下
; 「界面还在、后端没了」的半死状态。
; 交互模式问一句（取消 = 退出安装，不留拆一半的中间态）；/S 与 /P 没人可问，直接关。
;
; 实现约束：**不能把这段写成 Function**。模板在 include 本文件之后（第 87 行）
; 才 !addplugindir 加 nsis_tauri_utils 插件目录，函数体在 include 处立即汇编，
; 那时插件还不可见（Plugin not found）。宏只在 Section 里展开（第 611 行之后），
; 所以逻辑必须活在宏里。

Var HermixCmdOption

; 按名字结束当前用户下的同名进程（不存在则什么都不做，无日志噪音）。
; TAG 由调用方给：同一个宏在一次安装里被插入多次，标签必须互不相同。
!macro HermixKillByName TAG NAME
  nsis_tauri_utils::FindProcessCurrentUser "${NAME}"
  Pop $R0
  StrCmp $R0 0 0 hermix_kbn_${TAG}_done
  DetailPrint "Hermix Desktop：正在关闭残留进程 ${NAME}…"
  nsis_tauri_utils::KillProcessCurrentUser "${NAME}"
  Pop $R0
hermix_kbn_${TAG}_done:
!macroend

; ID 只用于本宏自己的标签：它会被 PREINSTALL 与 PREUNINSTALL 各插入一次。
!macro HermixStopLockingProcesses ID
  nsis_tauri_utils::FindProcessCurrentUser "${MAINBINARYNAME}.exe"
  Pop $R0
  StrCmp $R0 0 0 hermix_${ID}_app_not_running

  ; 主程序在跑：/S(silent) 与 /P(passive) 直接关（没人可问），其余问用户
  IfSilent hermix_${ID}_kill_app
  StrCpy $HermixCmdOption ""
  ${GetOptions} $CMDLINE "/P" $HermixCmdOption
  IfErrors 0 hermix_${ID}_kill_app
  MessageBox MB_OKCANCEL "Hermix Desktop 正在运行。$\n$\n继续安装会先关闭主程序和它拉起的后台进程（文件查看器、AI 内核、文件索引），然后覆盖程序文件。$\n$\n点「确定」继续安装，点「取消」退出。" IDOK hermix_${ID}_kill_app
  Quit

hermix_${ID}_kill_app:
  DetailPrint "Hermix Desktop：正在关闭运行中的 Hermix Desktop…"
  nsis_tauri_utils::KillProcessCurrentUser "${MAINBINARYNAME}.exe"
  Pop $R0
  Sleep 800 ; 等 Job Object 收树

hermix_${ID}_app_not_running:
  ; 索引助手是**提权进程**（读 MFT），KillProcess 会被「拒绝访问」挡回来；
  ; 它自带 `--stop`：走回环 shutdown 协议刷盘后自己退出。所以先按名杀，
  ; 杀不掉再请它走，最后再试一次强杀。
  !insertmacro HermixKillByName ${ID}_fs1 "hermix-filesearch.exe"
  nsis_tauri_utils::FindProcessCurrentUser "hermix-filesearch.exe"
  Pop $R0
  StrCmp $R0 0 0 hermix_${ID}_filesearch_done
  IfFileExists "$INSTDIR\hermix-filesearch.exe" 0 hermix_${ID}_filesearch_done
  DetailPrint "Hermix Desktop：正在请文件索引助手退出…"
  nsExec::ExecToLog '"$INSTDIR\hermix-filesearch.exe" --stop'
  Pop $R0
  Sleep 1500
  !insertmacro HermixKillByName ${ID}_fs2 "hermix-filesearch.exe"
hermix_${ID}_filesearch_done:

  !insertmacro HermixKillByName ${ID}_viewer "hermix-viewer.exe"
  !insertmacro HermixKillByName ${ID}_kernel "hermix.exe"
  !insertmacro HermixKillByName ${ID}_markitdown "markitdown.exe"
  !insertmacro HermixKillByName ${ID}_hxshell "hx-shell.exe"
!macroend

!macro NSIS_HOOK_PREINSTALL
  DetailPrint "Hermix Desktop：正在准备安装…"
  !insertmacro HermixStopLockingProcesses pre
!macroend

!macro NSIS_HOOK_POSTINSTALL
  DetailPrint "Hermix Desktop：正在校验安装完整性…"
  ; 主程序校验：杀毒软件静默隔离 exe 时用户只会看到打开即白屏，
  ; 这里把失败变成看得见的弹窗，而不是让用户自己猜。
  ; 主 exe 名 = cargo package name（hermix-desktop.exe），不是 productName。
  ; Tauri v2 的 mainBinaryName 默认取 src-tauri/Cargo.toml 的 [package] name，
  ; productName 只用于显示名/快捷方式/安装目录。此前这里按 productName 找
  ; "Hermix Desktop.exe"，在每台机器上都必然落空 → 恒定误报"主程序缺失"。
  ; 两种名字任一存在即算通过（与下方 sidecar 校验同策略），兼容 A 路便携包
  ; （release.sh 会把主 exe 改名为 productName.exe）。
  IfFileExists "$INSTDIR\hermix-desktop.exe" hermix_post_main_ok
  IfFileExists "$INSTDIR\Hermix Desktop.exe" hermix_post_main_ok hermix_post_main_fail
hermix_post_main_fail:
  DetailPrint "Hermix Desktop：完整性校验失败（主程序缺失）"
  MessageBox MB_ICONEXCLAMATION|MB_OK "Hermix Desktop 完整性校验失败：主程序缺失。$\n安装目录：$INSTDIR$\n$\n常见原因：杀毒软件隔离了安装文件，请检查隔离区后重新安装。"
  Goto hermix_post_sidecar_check
hermix_post_main_ok:
  DetailPrint "Hermix Desktop：主程序就绪。"
hermix_post_sidecar_check:
  IfFileExists "$INSTDIR\hermix.exe" hermix_post_sc_ok
  IfFileExists "$INSTDIR\hermix-x86_64-pc-windows-msvc.exe" hermix_post_sc_ok hermix_post_sc_fail
hermix_post_sc_fail:
  DetailPrint "Hermix Desktop：完整性校验失败（AI 内核缺失）"
  MessageBox MB_ICONEXCLAMATION|MB_OK "Hermix Desktop 完整性校验失败：AI 内核缺失。$\n安装目录：$INSTDIR$\n$\n常见原因：杀毒软件隔离了安装文件，请检查隔离区后重新安装。"
  Goto hermix_post_done
hermix_post_sc_ok:
  DetailPrint "Hermix Desktop：AI 内核就绪。"
hermix_post_done:
  ; 清理旧自研安装器残留的注册表项（若有）。
  ; 自研安装器曾在 HKCU\...Uninstall\Hermix 写项，若残留与当前 INSTDIR 不同，
  ; 旧的 UninstallString 仍会指向旧路径，导致「点卸载弹出旧安装器」。
  ; 这里只删键值不删文件（文件在 $INSTDIR 内，NSIS 卸载时会统一清理）。
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Hermix" "UninstallString"
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Hermix" "InstallLocation"
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  DetailPrint "Hermix Desktop：正在卸载…"
  ; 卸载同样要收进程：占着的 exe 删不掉，卸载器只会静默留下残骸。
  !insertmacro HermixStopLockingProcesses un
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  DetailPrint "Hermix Desktop：卸载完成。"
!macroend
