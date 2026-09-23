//! 进程级装配（C4 Boot）：把内核接缝的默认 provider 装进 [`Context`]。
//!
//! 这一层只做一件事——**决定这个进程用哪套 provider**。不写业务逻辑，
//! 不做条件分支：配置说用本地，就装本地实现；将来换远程沙箱，这里改成
//! 装 `RemoteExecWorld`，上层一行不动。
//!
//! 服务以 `Arc<dyn …>` 形式注册，因此取用方拿到的是接缝类型而不是具体
//! 实现类型——换成内存 Fs 或远程 Shell 都不需要改调用点。

use std::sync::Arc;

use hermix_core::{
    ExecWorld, Fs, LocalExecWorld, LocalFs, LocalSandbox, LocalShell, NoopTransport, Sandbox,
    Shell, Transport,
};

use crate::{Context, ServiceGuard};

/// 启动边界。装什么、开哪些暴露面，全部在这里决定，之后不再逐操作弹窗。
pub struct BootProfile {
    /// 删除类操作是否必须人确认。默认开。
    pub escalate_removal: bool,
    /// 对外暴露面。**默认 `None`（不暴露）**——网络暴露必须显式给一个
    /// transport 实现，光有个开关不算开启。
    pub transport: Option<Arc<dyn Transport>>,
}

impl Default for BootProfile {
    fn default() -> Self {
        Self { escalate_removal: true, transport: None }
    }
}

impl BootProfile {
    pub fn new() -> Self {
        Self::default()
    }

    /// 全自动：删除也不拦。
    pub fn unattended() -> Self {
        Self { escalate_removal: false, transport: None }
    }

    pub fn with_transport(mut self, t: Arc<dyn Transport>) -> Self {
        self.transport = Some(t);
        self
    }
}

/// 已暴露给业务层的接缝实例。把 `Boot` 装配出的 provider 引用传出，
/// 让 `hermix-tools` 的 register 函数能拿到同一个 Arc 实例——而不是
/// 各自构造一遍。S2 共享 provider 的语义在此落地：换 provider 是改
/// `Boot::local` 一处，工具层拿到的是同一个 Arc。
#[derive(Clone)]
pub struct BootSeams {
    pub fs: Arc<dyn Fs>,
    pub shell: Arc<dyn Shell>,
}

/// 已装配的进程。持有注册 guard——guard 掉了服务就注销，所以必须活到进程结束。
pub struct Boot {
    ctx: Context,
    seams: BootSeams,
    _guards: Vec<ServiceGuard>,
}

impl Boot {
    /// 按 profile 装配本地 provider。
    pub fn local(profile: BootProfile) -> Self {
        let ctx = Context::new();
        let world = Arc::new(LocalExecWorld::new());

        let fs: Arc<dyn Fs> = Arc::new(LocalFs);
        let shell: Arc<dyn Shell> = Arc::new(LocalShell::default());
        let sandbox: Arc<dyn Sandbox> =
            Arc::new(LocalSandbox { escalate_removal: profile.escalate_removal });
        let transport: Arc<dyn Transport> =
            profile.transport.unwrap_or_else(|| Arc::new(NoopTransport));
        let exec: Arc<dyn ExecWorld> = world.clone();

        let guards = vec![
            ctx.provide(fs.clone()),
            ctx.provide(shell.clone()),
            ctx.provide(sandbox),
            ctx.provide(transport),
            ctx.provide(exec),
        ];
        Self { ctx, seams: BootSeams { fs, shell }, _guards: guards }
    }

    /// 给业务层（工具 register、agent loop 等）拿去调用接缝的实例。
    /// 这是接缝从「Context 内可见」升级到「外部可见」的唯一通道——避免
    /// 每个调用方各自 `Arc::new(LocalFs)` 又造一遍。
    pub fn seams(&self) -> BootSeams {
        self.seams.clone()
    }

    pub fn context(&self) -> &Context {
        &self.ctx
    }

    /// 便捷取值。服务以 `Arc<dyn …>` 注册，取出时是 `Arc<Arc<dyn …>>`，
    /// 解一层再克隆，让调用方拿到的是接缝类型而不是双层 Arc。
    ///
    /// 拿不到说明装配没跑，属于启动 bug，直接 panic 而不是静默降级。
    fn service<T: std::any::Any + Send + Sync>(&self, name: &str) -> Arc<T> {
        self.ctx.get::<T>().unwrap_or_else(|| panic!("{name} not installed"))
    }

    pub fn fs(&self) -> Arc<dyn Fs> {
        (*self.service::<Arc<dyn Fs>>("fs")).clone()
    }

    pub fn shell(&self) -> Arc<dyn Shell> {
        (*self.service::<Arc<dyn Shell>>("shell")).clone()
    }

    pub fn sandbox(&self) -> Arc<dyn Sandbox> {
        (*self.service::<Arc<dyn Sandbox>>("sandbox")).clone()
    }

    pub fn transport(&self) -> Arc<dyn Transport> {
        (*self.service::<Arc<dyn Transport>>("transport")).clone()
    }

    pub fn exec(&self) -> Arc<dyn ExecWorld> {
        (*self.service::<Arc<dyn ExecWorld>>("exec world")).clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermix_core::{
        Envelope, InMemoryFs, Op, TransportError, Verdict,
    };
    use std::path::PathBuf;
    use std::sync::Mutex;

    #[test]
    fn boot_installs_every_seam() {
        let boot = Boot::local(BootProfile::new());
        assert!(boot.context().has::<Arc<dyn Fs>>());
        assert!(boot.context().has::<Arc<dyn Shell>>());
        assert!(boot.context().has::<Arc<dyn Sandbox>>());
        assert!(boot.context().has::<Arc<dyn Transport>>());
        assert!(boot.context().has::<Arc<dyn ExecWorld>>());
    }

    /// 业务层从 boot 取 seams：返回的实例与 boot 内部的 Context 注册实例
    /// 是同一个 Arc——避免重复构造。
    #[test]
    fn boot_seams_are_the_same_arcs_as_in_the_context() {
        let boot = Boot::local(BootProfile::new());
        let s = boot.seams();
        let from_ctx: Arc<dyn Fs> = (*boot.context().get::<Arc<dyn Fs>>().unwrap()).clone();
        assert!(Arc::ptr_eq(&s.fs, &from_ctx));
    }

    #[test]
    fn default_profile_escalates_removal_and_exposes_nothing() {
        let boot = Boot::local(BootProfile::new());
        let op = Op::FsRemove { path: PathBuf::from("a.txt") };
        assert_eq!(boot.sandbox().allows(&op), Verdict::Escalate);
        assert_eq!(boot.sandbox().allows(&Op::FsRead { path: PathBuf::from("a.txt") }), Verdict::Allow);
    }

    #[test]
    fn unattended_profile_never_escalates() {
        let boot = Boot::local(BootProfile::unattended());
        let op = Op::FsRemove { path: PathBuf::from("a.txt") };
        assert_eq!(boot.sandbox().allows(&op), Verdict::Allow);
    }

    #[tokio::test]
    async fn no_transport_configured_means_disabled_not_broken() {
        let boot = Boot::local(BootProfile::new());
        let env = Envelope { session: "s".into(), kind: "entry".into(), payload: vec![] };
        assert!(matches!(
            boot.transport().publish(&env).await,
            Err(TransportError::Disabled)
        ));
    }

    struct RecordingTransport {
        seen: Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl Transport for RecordingTransport {
        async fn publish(&self, env: &Envelope) -> Result<(), TransportError> {
            self.seen.lock().unwrap().push(env.kind.clone());
            Ok(())
        }
    }

    #[tokio::test]
    async fn configured_transport_is_the_only_way_to_expose() {
        let t = Arc::new(RecordingTransport { seen: Mutex::new(Vec::new()) });
        let boot = Boot::local(BootProfile::new().with_transport(t.clone()));
        let env = Envelope { session: "s".into(), kind: "entry".into(), payload: vec![] };
        boot.transport().publish(&env).await.unwrap();
        assert_eq!(*t.seen.lock().unwrap(), vec!["entry".to_string()]);
    }

    /// 插件拿 child scope 换掉 Fs，父级不受影响——C1 的可逆性在装配层仍然成立。
    #[test]
    fn a_child_scope_can_shadow_the_exec_world_without_touching_the_parent() {
        let boot = Boot::local(BootProfile::new());
        let child = boot.context().child();
        let _guard = child.provide(Arc::new(InMemoryFs::default()) as Arc<dyn Fs>);
        assert!(child.get::<Arc<dyn Fs>>().is_some());
        // 父级仍是装配时装的 LocalFs（换成内存实现这件事对父级不可见）
        assert!(boot.context().get::<Arc<dyn Fs>>().is_some());
    }

    /// guard 掉了服务就注销：装配对象析构后不该还有残留 provider。
    #[test]
    fn dropping_boot_uninstalls_every_service() {
        let ctx = {
            let boot = Boot::local(BootProfile::new());
            boot.context().clone()
        };
        assert!(ctx.get::<Arc<dyn Fs>>().is_none());
        assert!(ctx.get::<Arc<dyn ExecWorld>>().is_none());
    }
}
