//! 代理模块日志错误码定义
//!
//! 格式: [模块-编号] 消息
//! - CB: Circuit Breaker (熔断器)
//! - SRV: Server (服务器)
//! - FWD: Forwarder (转发器)
//! - KEY: API Key pool (Key 池)

/// 熔断器日志码
pub mod cb {
    pub const OPEN_TO_HALF_OPEN: &str = "CB-001";
    pub const HALF_OPEN_TO_CLOSED: &str = "CB-002";
    pub const HALF_OPEN_PROBE_FAILED: &str = "CB-003";
    pub const TRIGGERED_FAILURES: &str = "CB-004";
    pub const TRIGGERED_ERROR_RATE: &str = "CB-005";
    pub const MANUAL_RESET: &str = "CB-006";
}

/// 服务器日志码
pub mod srv {
    pub const STARTED: &str = "SRV-001";
    pub const STOPPED: &str = "SRV-002";
    pub const STOP_TIMEOUT: &str = "SRV-003";
    pub const TASK_ERROR: &str = "SRV-004";
    pub const ACCEPT_ERR: &str = "SRV-005";
    pub const CONN_ERR: &str = "SRV-006";
}

/// 转发器日志码
pub mod fwd {
    pub const PROVIDER_FAILED_RETRY: &str = "FWD-001";
    pub const ALL_PROVIDERS_FAILED: &str = "FWD-002";
    pub const SINGLE_PROVIDER_FAILED: &str = "FWD-003";
}

/// Key 池日志码
pub mod key {
    /// 当前 Key 可重试失败，改用同一供应商池内的下一个 Key
    pub const RETRY_NEXT_KEY: &str = "KEY-001";
    /// 同一供应商的 Key 池已用尽，转到供应商级故障转移
    pub const POOL_EXHAUSTED: &str = "KEY-002";
}
