// lib.rs — 供 loader 主程序与 inject_app 打包工具共享的模块

pub mod payload;
pub mod capability;
pub mod bridge;

use std::sync::OnceLock;

static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

/// 供 bridge 在同步的协议处理器线程里 block_on 异步操作 (exec/fetch/dialog).
pub fn rt() -> &'static tokio::runtime::Runtime {
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("创建 tokio runtime 失败")
    })
}
