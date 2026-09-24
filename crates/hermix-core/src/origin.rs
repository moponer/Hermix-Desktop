//! 来源感知权限（FOUNDATION_REWRITE_DESIGN §5.5 第 5 层）。
//!
//! 来源由服务端根据连接属性判定，客户端无法声明（对策 T8 提权）。
//! 权限矩阵是唯一权威：审批引擎、HTTP 中间件都从 [`Origin::may`]
//! 取决策，不允许各自维护第二份表。

/// 请求来源。
///
/// `Default = Local`：未显式设置时退回本地（最宽松），
/// 与 `ToolCallContext::default()` 的"agent loop 桌面 WS 始终 Local"语义一致。
/// HTTP 路径会由 `security::request_guard` 显式覆盖为实际推导结果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Origin {
    /// 桌面端自身发起的请求（用户直接操作）。
    #[default]
    Local,
    /// 已配对设备（手机端）发起的请求。
    PairedDevice {
        /// 服务端签发的设备 id，用于审计与撤销。
        device_id: String,
    },
    /// 未配对来源。只能访问 /health 与配对端点；任何能力层调用都拒绝。
    Unpaired,
}

/// 能力分类。粒度对齐设计文档 §5.5 权限矩阵，不按具体工具列——
/// 具体工具→能力的映射由调用方（审批引擎 / HTTP 路由）负责。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    /// 发消息 / 看会话。
    Chat,
    /// 对工具调用做"本次会话总是允许"级别的授权。
    ApproveAlways,
    /// 执行 shell / 删除类等高危工具。
    DangerousTool,
    /// 修改配置 / 模型。
    ConfigMutation,
    /// 设备配对 / 撤销。
    DeviceManagement,
    /// 访问工作台 / 自定义 App。
    WorkbenchAccess,
}

/// 权限判定结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// 直接允许。
    Allowed,
    /// 本来源不能自行放行，必须桌面端（Local）人工确认。
    NeedsLocalConfirmation,
    /// 拒绝。
    Denied,
}

impl Origin {
    /// 权限矩阵（§5.5，默认值）。规则：
    /// - Local 全允许；
    /// - PairedDevice 默认只读 + 聊天，高危操作强制桌面确认，
    ///   授权类 / 配置类 / 管理类一律拒绝；
    /// - Unpaired 一律拒绝（纵深防御：即使 HTTP 层漏拦也到不了执行）。
    pub fn may(&self, cap: Capability) -> Decision {
        match self {
            Origin::Local => Decision::Allowed,
            Origin::Unpaired => Decision::Denied,
            Origin::PairedDevice { .. } => match cap {
                Capability::Chat => Decision::Allowed,
                Capability::DangerousTool => Decision::NeedsLocalConfirmation,
                Capability::ApproveAlways
                | Capability::ConfigMutation
                | Capability::DeviceManagement
                | Capability::WorkbenchAccess => Decision::Denied,
            },
        }
    }

    /// 审计用：设备 id（Local / Unpaired 返回 None）。
    pub fn device_id(&self) -> Option<&str> {
        match self {
            Origin::PairedDevice { device_id } => Some(device_id),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_is_allowed_for_every_capability() {
        for cap in [
            Capability::Chat,
            Capability::ApproveAlways,
            Capability::DangerousTool,
            Capability::ConfigMutation,
            Capability::DeviceManagement,
            Capability::WorkbenchAccess,
        ] {
            assert_eq!(Origin::Local.may(cap), Decision::Allowed, "cap: {cap:?}");
        }
    }

    #[test]
    fn unpaired_is_denied_for_every_capability() {
        for cap in [
            Capability::Chat,
            Capability::ApproveAlways,
            Capability::DangerousTool,
            Capability::ConfigMutation,
            Capability::DeviceManagement,
            Capability::WorkbenchAccess,
        ] {
            assert_eq!(Origin::Unpaired.may(cap), Decision::Denied, "cap: {cap:?}");
        }
    }

    #[test]
    fn paired_device_follows_default_matrix() {
        let remote = Origin::PairedDevice { device_id: "d1".into() };
        // 可聊、高危需桌面确认。
        assert_eq!(remote.may(Capability::Chat), Decision::Allowed);
        assert_eq!(remote.may(Capability::DangerousTool), Decision::NeedsLocalConfirmation);
        // 授权 / 配置 / 管理 / 工作台一律拒绝。
        assert_eq!(remote.may(Capability::ApproveAlways), Decision::Denied);
        assert_eq!(remote.may(Capability::ConfigMutation), Decision::Denied);
        assert_eq!(remote.may(Capability::DeviceManagement), Decision::Denied);
        assert_eq!(remote.may(Capability::WorkbenchAccess), Decision::Denied);
    }

    #[test]
    fn default_origin_is_local() {
        // ToolCallContext::default() 依赖此语义：未显式设置 ⇒ 桌面直连。
        assert_eq!(Origin::default(), Origin::Local);
    }

    #[test]
    fn device_id_only_visible_on_paired_origin() {
        assert_eq!(Origin::Local.device_id(), None);
        assert_eq!(Origin::Unpaired.device_id(), None);
        assert_eq!(
            Origin::PairedDevice { device_id: "d1".into() }.device_id(),
            Some("d1")
        );
    }
}
