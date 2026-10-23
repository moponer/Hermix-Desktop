; hooks.nsh — Tauri NSIS 安装器钩子
; 四个宏会被 Tauri 模板无条件 !insertmacro，全部必须定义（缺一个编译即失败）。
; 校验用宽容匹配：sidecar 落盘文件名是否剥三元组后缀取决于 bundler 行为，
; 两种名字任一存在即算通过，避免把"名字对不上"误报成"文件被杀"。

!macro NSIS_HOOK_PREINSTALL
  DetailPrint "Hermix Desktop：正在准备安装…"
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
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  DetailPrint "Hermix Desktop：卸载完成。"
!macroend
