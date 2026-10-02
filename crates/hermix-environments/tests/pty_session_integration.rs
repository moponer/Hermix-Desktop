//! H3 持久终端会话集成测试 —— 真实 ConPTY + PowerShell。
//!
//! 验收标准来自立项笔记（H3 原始目标）：cd / venv / REPL 状态跨调用存活。
//! 每个测试各自 `LocalBackend::default()`（独立会话表，互不污染）。
//! 会 spawn 真实 powershell（冷启 2-4s），预算放宽到 30s。

use hermix_core::TerminalBackend;
use hermix_environments::LocalBackend;

const BUDGET: Option<u64> = Some(30_000);

#[tokio::test]
async fn persistent_session_keeps_cwd_across_calls() {
    let backend = LocalBackend::default();
    let sid = backend
        .pty_create_session(None)
        .await
        .expect("create session");

    let out = backend
        .pty_send_session(&sid, "cd C:\\Windows", true, BUDGET)
        .await
        .expect("cd");
    assert!(out.alive, "session should stay alive after cd");

    // H3 核心验收：新的一次性调用会丢掉 cwd；持久会话必须记住。
    let out = backend
        .pty_send_session(&sid, "(Get-Location).Path", true, BUDGET)
        .await
        .expect("pwd");
    assert!(
        out.text.contains("C:\\Windows"),
        "cwd did not survive across calls, got: {:?}",
        out.text
    );

    backend.pty_close_session(&sid).await.expect("close");
}

#[tokio::test]
async fn persistent_session_strips_marker_and_returns_output() {
    let backend = LocalBackend::default();
    let sid = backend.pty_create_session(None).await.expect("create");

    let out = backend
        .pty_send_session(&sid, "Write-Output 'pty-integration-42'", true, BUDGET)
        .await
        .expect("send");
    assert!(
        out.text.contains("pty-integration-42"),
        "output missing, got: {:?}",
        out.text
    );
    assert!(
        !out.text.contains("HERMIXPTY-"),
        "marker leaked into model-visible text: {:?}",
        out.text
    );
    assert!(!out.busy, "short command must not be flagged busy");

    backend.pty_close_session(&sid).await.expect("close");
}

#[tokio::test]
async fn persistent_session_keeps_variable_state_across_calls() {
    let backend = LocalBackend::default();
    let sid = backend.pty_create_session(None).await.expect("create");

    // 分片提交（submit=false 续行）+ 最终提交 —— REPL 式交互路径。
    backend
        .pty_send_session(&sid, "$x = 40 +", false, BUDGET)
        .await
        .expect("fragment");
    backend
        .pty_send_session(&sid, "2", true, BUDGET)
        .await
        .expect("finish");
    let out = backend
        .pty_send_session(&sid, "$x", true, BUDGET)
        .await
        .expect("read var");
    assert!(
        out.text.contains("42"),
        "variable state did not survive (continuation fragments), got: {:?}",
        out.text
    );

    backend.pty_close_session(&sid).await.expect("close");
}

#[tokio::test]
async fn oneshot_pty_executes_and_reports_exit_code() {
    let backend = LocalBackend::default();
    let out = backend
        .execute_command(
            "Write-Output 'pty-oneshot-42'; exit 7",
            Some(60),
            None,
            false,
            true,
        )
        .await
        .expect("oneshot pty");
    assert_eq!(out.exit_code, 7, "oneshot exit code must propagate");
    assert!(
        out.stdout.contains("pty-oneshot-42"),
        "oneshot output missing, got: {:?}",
        out.stdout
    );
}

#[tokio::test]
async fn unknown_session_and_close_unknown_error() {
    let backend = LocalBackend::default();
    let err = backend
        .pty_send_session("pty-999", "x", true, Some(5_000))
        .await
        .expect_err("unknown session must error");
    assert!(err.to_string().contains("unknown persistent session"));

    let err = backend
        .pty_close_session("pty-999")
        .await
        .expect_err("close unknown must error");
    assert!(err.to_string().contains("unknown persistent session"));
}

#[tokio::test]
async fn list_and_close_session_lifecycle() {
    let backend = LocalBackend::default();
    let sid = backend.pty_create_session(None).await.expect("create");

    let infos = backend.pty_list_sessions().await;
    assert!(
        infos.iter().any(|i| i.session_id == sid && i.alive),
        "created session must be listed alive, got: {infos:?}"
    );

    backend.pty_close_session(&sid).await.expect("close");
    let infos = backend.pty_list_sessions().await;
    assert!(
        !infos.iter().any(|i| i.session_id == sid),
        "closed session must be gone, got: {infos:?}"
    );
}
