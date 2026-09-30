//! 测试专用的进程环境隔离。
//!
//! `HERMIX_HOME` 是**进程级全局**：`hermix_config::paths::hermix_home()`
//! 每次都现读环境变量。而 Rust 的测试默认多线程并行 —— 于是"某个测试把
//! `HERMIX_HOME` 指到临时目录"会外溢到同时跑的其他测试身上：目录随
//! tempdir 一起被删，正在写证书/设备的测试就报
//! `rename ... cert.pem: 系统找不到指定的文件 (os error 2)`。
//!
//! 本模块给这类测试两样东西：
//! 1. [`env_lock`] —— 跨线程互斥，保证"改环境变量"这一段不与别的测试交错；
//! 2. [`EnvGuard`] —— RAII：进入时记下旧值，drop 时原样还原（含"原先没有
//!    这个变量"的情况），杜绝"临时目录删了、环境变量还指着它"的悬垂状态。
//!
//! 用法：
//! ```ignore
//! let _guard = hermix_server::test_env::EnvGuard::set("HERMIX_HOME", home.path());
//! ```
//! 守卫本身已持锁，drop 时先还原再放锁，不需要另拿 [`env_lock`]。

use std::sync::{Mutex, MutexGuard, OnceLock};

/// 进程内所有"改 `HERMIX_HOME`"的测试共用一把锁。
pub fn env_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// RAII 环境变量守卫：设置 → 离开作用域时还原，并全程持有 [`env_lock`]。
pub struct EnvGuard {
    key: &'static str,
    prev: Option<std::ffi::OsString>,
    _lock: MutexGuard<'static, ()>,
}

impl EnvGuard {
    /// 设置环境变量并返回守卫。`value` 接受任何能转成字符串的值。
    pub fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let lock = env_lock();
        let prev = std::env::var_os(key);
        // SAFETY：已持有 env_lock —— 本进程内所有改该类全局的测试都被串行化，
        // 不会与其它线程的 set_var/remove_var 并发（后者在多线程下是 UB）。
        unsafe { std::env::set_var(key, value) };
        Self { key, prev, _lock: lock }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY：见 `set` —— 锁还持有（drop 字段在方法体之后才跑），
        // 期间没有别的线程碰同一个变量。
        unsafe {
            match self.prev.take() {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }
}
