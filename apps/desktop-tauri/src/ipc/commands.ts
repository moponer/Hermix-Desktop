/**
 * Tauri command 名常量表。
 *
 * 为什么需要：命令名在 `invoke("...")` 里是**裸字符串**，改 Rust 侧
 * `#[tauri::command]` 的函数名时，TS 编译器一句话都不会说，只在运行期变成
 * "Command not found"。常量表把这类改动变成一次可搜索的重命名。
 *
 * 约定：新增命令先在这里加，再在调用点用 `IPC.xxx`；不要直接写字符串。
 * 值必须与 `src-tauri/src/commands.rs` 的函数名逐字一致。
 */
export const IPC = {
  // 传输层
  backendHttpRequest: "backend_http_request",
  backendReady: "backend_ready",
  backendDiagnosis: "backend_diagnosis",
  backendChannelSend: "backend_channel_send",
  getServerStatus: "get_server_status",
  restartSidecar: "restart_sidecar",
  restartApp: "restart_app",

  // 运行时 / 系统
  getRuntimeInfo: "get_runtime_info",
  getDataDir: "get_data_dir",
  openExternalUrl: "open_external_url",
  setKeepAwake: "set_keep_awake",
  sendNotification: "send_notification",
  setAutostart: "set_autostart",
  getAutostart: "get_autostart",
  setCloseAction: "set_close_action",
  syncTray: "sync_tray",
  traySetTaskCount: "tray_set_task_count",
  getDesktopSettings: "get_desktop_settings",

  // 文件 / 产物
  readArtifactFile: "read_artifact_file",
  readArtifactFileBytes: "read_artifact_file_bytes",
  artifactPathExists: "artifact_path_exists",
  documentToMarkdown: "document_to_markdown",
  listDirectory: "list_directory",

  // 日志
  listBackendLogFiles: "list_backend_log_files",
  readBackendLogTail: "read_backend_log_tail",
  openLogDirectory: "open_log_directory",

  // 窗口 / 截图
  openInViewer: "open_in_viewer",
  startScreenshot: "start_screenshot",
  screenshotOverlayReady: "screenshot_overlay_ready",
  screenshotWindowAt: "screenshot_window_at",
  screenshotSelect: "screenshot_select",
  screenshotCancel: "screenshot_cancel",
  screenshotCommitEdited: "screenshot_commit_edited",
  screenshotBackdropHide: "screenshot_backdrop_hide",
  screenshotSessionInfo: "screenshot_session_info",
  screenshotApplyConfig: "screenshot_apply_config",
  screenshotHotkeyStatus: "screenshot_hotkey_status",

  // 运行时 / 更新 / 环境
  updateCheck: "update_check",
  updateDownload: "update_download",
  updateInstall: "update_install",
  bootStage: "boot_stage",
  pythonDetect: "python_detect",
  setPythonOverride: "set_python_override",
  runtimeDetect: "runtime_detect",
  fileAssocList: "file_assoc_list",
  fileAssocSet: "file_assoc_set",
  fileSearchApplyConfig: "file_search_apply_config",
  reportFrontendError: "report_frontend_error",

  // 自定义应用
  customAppValidate: "custom_app_validate",
  customAppSmokeTest: "custom_app_smoke_test",

  // 工作台（沙箱化预览）
  workbenchCreate: "workbench_create",
  workbenchList: "workbench_list",
  workbenchRead: "workbench_read",
  workbenchWrite: "workbench_write",
  workbenchDelete: "workbench_delete",
  workbenchFiles: "workbench_files",
  workbenchFsList: "workbench_fs_list",
  workbenchFsRead: "workbench_fs_read",
  workbenchFsWrite: "workbench_fs_write",
  workbenchHistory: "workbench_history",
  workbenchUndo: "workbench_undo",
  workbenchLint: "workbench_lint",
  workbenchModuleIndex: "workbench_module_index",
  workbenchSetDeps: "workbench_set_deps",
  workbenchSetPrimary: "workbench_set_primary",
  workbenchVendorDir: "workbench_vendor_dir",
  workbenchEntryPath: "workbench_entry_path",
  workbenchBindSession: "workbench_bind_session",
  workbenchEnsureSession: "workbench_ensure_session",
  workbenchDbQuery: "workbench_db_query",
  workbenchDbExec: "workbench_db_exec",
} as const;

export type IpcCommand = (typeof IPC)[keyof typeof IPC];
