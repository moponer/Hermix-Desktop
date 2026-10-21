//! 建索引进度回调。
//!
//! 建索引是**分钟级**的长任务（MFT 枚举 + 逐文件读体积 + 落盘快照）。
//! 没有进度，用户看到的就只是"卡住了"——这正是旧实现的问题：UI 只能显示
//! 一个转圈。这里把回调贯穿枚举 / 测体积 / 建表 / 落盘四个阶段，UI 拿到的
//! 是真实数字（已处理条目数、已累计字节、已用时），不是假动效。

use std::sync::Arc;

use crate::core::model::BuildProgress;

/// 进度回调。`None` = 不关心进度（测试与手工跑批时用）。
pub type BuildProgressFn = Arc<dyn Fn(BuildProgress) + Send + Sync>;
