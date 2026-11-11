//! 请求转发器
//!
//! 负责将请求转发到上游Provider，支持故障转移

use super::hyper_client::{ProxyResponse, MAX_RESPONSE_BODY_BYTES};
use super::{
    body_filter::{contains_private_params, strip_private_params},
    content_encoding::{decoded_body, get_content_encoding},
    error::*,
    failover_switch::FailoverSwitchManager,
    log_codes::{fwd as log_fwd, key as log_key},
    opaque_state_rectifier::{
        carries_encrypted_agent_task, detect_opaque_state_rejection, rectify_opaque_state,
        unreadable_agent_task_error, OpaqueStateRejection,
    },
    provider_router::ProviderRouter,
    providers::{
        codex_chat_history::CodexChatHistoryStore, gemini_shadow::GeminiShadowStore, get_adapter,
        AuthInfo, AuthStrategy, ProviderAdapter, ProviderType,
    },
    thinking_budget_rectifier::{rectify_thinking_budget, should_rectify_thinking_budget},
    thinking_rectifier::{rectify_anthropic_request, should_rectify_thinking_signature},
    types::{CopilotOptimizerConfig, OptimizerConfig, ProxyStatus, RectifierConfig},
    ProxyError,
};
use crate::commands::{CodexOAuthState, CopilotAuthState, XaiOAuthState};
use crate::proxy::json_canonical::short_value_hash;
use crate::proxy::providers::copilot_auth::CopilotAuthError;
use crate::proxy::providers::copilot_model_map::{
    CopilotProtocol, CopilotTransport, ResolvedCopilotModel,
};
use crate::proxy::sse::Utf8TailDecoder;
use crate::{
    app_config::AppType,
    provider::{CodexCopilotApiFormat, LocalProxyRequestOverrides, Provider},
};
use bytes::Bytes;
use futures::StreamExt;
use http::Extensions;
use serde_json::Value;
use sha2::{Digest, Sha256};
use smallvec::{smallvec, SmallVec};
use std::borrow::Cow;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tauri::Manager;
use tokio::sync::RwLock;

const PROXY_AUTH_PLACEHOLDER: &str = "PROXY_MANAGED";

/// 跨请求生命周期共享的 Provider。避免在上下文、成功结果和错误回填间深拷贝配置。
pub type SharedProvider = Arc<Provider>;

/// 已解码但未被 Stack 路由改写的原始请求体。
///
/// handler 只有在 body 仍与客户端发来的字节一致时才放入扩展；forwarder 只有在
/// 整条改写管道都没把请求体变成 Owned 时才复用它，避免 JSON Value 二次序列化。
#[derive(Clone)]
pub(crate) struct RawRequestBody {
    pub(crate) bytes: Bytes,
}

/// Whether the upstream belongs to the OpenCode gateway (`opencode.ai`).
///
/// That gateway (<https://opencode.ai/docs/go/>) expects clients to identify
/// themselves with a `User-Agent` and to carry a **stable per-conversation**
/// session id in `x-opencode-session`; a request without the session header is
/// rejected with `400 MissingSessionID`. Claude Code and Codex already send
/// headers the gateway recognises, Claude Desktop does not - the proxy fills
/// the gap (see the injection site in `forward`).
fn is_opencode_upstream(upstream_host: Option<&str>) -> bool {
    upstream_host
        .map(|host| {
            let host = host.rsplit_once(':').map_or(host, |(h, _)| h);
            host.trim_matches(['[', ']']).to_ascii_lowercase()
        })
        .is_some_and(|host| host == "opencode.ai" || host.ends_with(".opencode.ai"))
}

/// Derive a session id that stays **stable within one conversation**: a digest
/// of the system prompt plus the first user message.
///
/// The gateway uses the session id for upstream routing and prompt caching, so
/// it must not change between the turns of a conversation. Claude Desktop hands
/// CC Switch a fresh id on every request, which defeats that; hashing the
/// conversation prefix keeps the id stable while still separating conversations.
fn conversation_fingerprint(body: &Value) -> Option<String> {
    let mut hasher = Sha256::new();
    let mut fed = false;
    if let Some(system) = body.get("system") {
        write_json_without_cache_control(system, &mut hasher);
        fed = true;
    }
    if let Some(first_user) = body
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|messages| {
            messages
                .iter()
                .find(|m| m.get("role").and_then(Value::as_str) == Some("user"))
        })
    {
        write_json_without_cache_control(first_user, &mut hasher);
        fed = true;
    }
    if !fed {
        return None;
    }
    let digest = format!("{:x}", hasher.finalize());
    Some(format!("ccsw-{}", &digest[..32]))
}

/// A stable session header the client already sends and the gateway recognises
/// natively (Claude Code, Codex). Checked before deriving an id of our own.
fn native_session_header(headers: &http::HeaderMap) -> Option<&str> {
    [
        "x-claude-code-session-id",
        "session-id",
        "session_id",
        "x-session-id",
    ]
    .iter()
    .find_map(|name| {
        headers
            .get(*name)?
            .to_str()
            .ok()
            .map(str::trim)
            .filter(|value| !value.is_empty())
    })
}

/// Pick the value for `x-opencode-session`, most stable identity first: a native
/// session header the client already sends, the client-provided session id, the
/// conversation fingerprint, then the generated id.
fn pick_opencode_session<'a>(
    native: Option<&'a str>,
    client_provided: Option<&'a str>,
    fingerprint: Option<&'a str>,
    generated: &'a str,
) -> Option<String> {
    [native, client_provided, fingerprint, Some(generated)]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|value| !value.is_empty())
        .map(str::to_string)
}

/// Clone `value` without any `cache_control` key, at any depth.
///
/// Clients move that marker between messages as the conversation grows (Claude
/// Code puts it on the first user message, then on the last one), so hashing it
/// would change the session id once, mid-conversation.
fn write_json_without_cache_control(value: &Value, hasher: &mut Sha256) {
    match value {
        Value::Object(map) => {
            hasher.update(b"{");
            for (index, (key, child)) in map
                .iter()
                .filter(|(key, _)| key.as_str() != "cache_control")
                .enumerate()
            {
                if index > 0 {
                    hasher.update(b",");
                }
                serde_json::to_writer(&mut *hasher, key).expect("serialize object key");
                hasher.update(b":");
                write_json_without_cache_control(child, hasher);
            }
            hasher.update(b"}");
        }
        Value::Array(items) => {
            hasher.update(b"[");
            for (index, child) in items.iter().enumerate() {
                if index > 0 {
                    hasher.update(b",");
                }
                write_json_without_cache_control(child, hasher);
            }
            hasher.update(b"]");
        }
        other => serde_json::to_writer(&mut *hasher, other).expect("serialize JSON value"),
    }
}

fn codex_bearer_access_token(headers: &http::HeaderMap) -> Option<&str> {
    let authorization = headers
        .get(http::header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .trim();
    let mut parts = authorization.split_whitespace();
    let scheme = parts.next()?;
    let token = parts.next()?;
    if !scheme.eq_ignore_ascii_case("bearer") || token.is_empty() || parts.next().is_some() {
        return None;
    }
    Some(token)
}

fn serialize_request_body(value: &Value, initial_capacity: usize) -> Result<Bytes, ProxyError> {
    let mut bytes = Vec::with_capacity(initial_capacity);
    serde_json::to_writer(&mut bytes, value)
        .map_err(|e| ProxyError::Internal(format!("Failed to serialize request body: {e}")))?;
    Ok(Bytes::from(bytes))
}

fn validate_codex_official_authorization(
    headers: &http::HeaderMap,
    provider: &Provider,
    expected_chatgpt_account_id: Option<&str>,
    managed_session_matches: Option<bool>,
) -> Result<(), ProxyError> {
    let authorization = headers
        .get(http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(str::trim);
    match authorization {
        None | Some("") => Err(ProxyError::AuthError(
            "Codex 官方登录不可用，请先在 Codex 中完成 ChatGPT 登录".to_string(),
        )),
        // 带占位 Key 的请求有两种来源：没重载配置的 Codex 进程；或切换前用第三方供应商建的
        // 旧会话（Codex 按会话记下的 provider 走休眠的 custom 表，重启也还是它）。
        Some(value) if value.contains(PROXY_AUTH_PLACEHOLDER) => Err(ProxyError::AuthError(
            "已切换到 OpenAI 官方供应商，但这个请求仍按第三方供应商的配置发出。如果这是切换前用第三方供应商创建的旧会话，它会一直沿用原供应商，重启也无效，请新建会话继续；否则请重启 Codex 以加载官方登录配置".to_string(),
        )),
        Some(_) => {
            let managed_account_id = provider
                .meta
                .as_ref()
                .and_then(|meta| meta.managed_account_id_for("codex_oauth"))
                .map(|account_id| account_id.trim().to_string())
                .filter(|account_id| !account_id.is_empty());
            if managed_account_id.is_some() {
                let request_account_id = headers
                    .get("chatgpt-account-id")
                    .and_then(|value| value.to_str().ok())
                    .map(str::trim)
                    .filter(|account_id| !account_id.is_empty());
                if request_account_id != expected_chatgpt_account_id
                    || managed_session_matches != Some(true)
                {
                    return Err(ProxyError::AuthError(
                        "当前 Codex 会话未加载所选 ChatGPT 账号，请重启 Codex 或新建会话后重试"
                            .to_string(),
                    ));
                }
            }
            Ok(())
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexUpstreamFormat {
    NativeResponses,
    ChatCompletions,
    Anthropic,
}

struct CopilotRequestContext {
    classification: super::copilot_optimizer::CopilotClassification,
    request_id: Option<String>,
    interaction_id: Option<String>,
}

impl CopilotRequestContext {
    fn apply_headers(
        &self,
        headers: &mut Vec<(http::HeaderName, http::HeaderValue)>,
        request_classification: bool,
    ) -> Result<(), ProxyError> {
        let request_id = self
            .request_id
            .as_deref()
            .map(http::HeaderValue::from_str)
            .transpose()
            .map_err(|_| ProxyError::ConfigError("Invalid Copilot request ID".to_string()))?;
        let interaction_id = self
            .interaction_id
            .as_deref()
            .map(http::HeaderValue::from_str)
            .transpose()
            .map_err(|_| ProxyError::ConfigError("Invalid Copilot interaction ID".to_string()))?;

        for (name, value) in headers.iter_mut() {
            match name.as_str() {
                "x-initiator" if request_classification => {
                    *value = http::HeaderValue::from_static(self.classification.initiator);
                }
                "x-interaction-type" if self.classification.is_subagent => {
                    *value = http::HeaderValue::from_static("conversation-subagent");
                }
                "x-request-id" | "x-agent-task-id" => {
                    if let Some(request_id) = &request_id {
                        *value = request_id.clone();
                    }
                }
                _ => {}
            }
        }

        if let Some(interaction_id) = interaction_id {
            headers.push((
                http::HeaderName::from_static("x-interaction-id"),
                interaction_id,
            ));
        }
        if self.classification.is_subagent {
            log::info!(
                "[Copilot] 子代理请求: x-initiator=agent, x-interaction-type=conversation-subagent"
            );
        }
        Ok(())
    }
}

pub struct ForwardResult {
    pub response: ProxyResponse,
    pub provider: SharedProvider,
    /// SHA-256 fingerprint of the key-pool entry used by the successful try.
    pub api_key_id: Option<String>,
    pub claude_api_format: Option<String>,
    /// Wire format selected for this specific Codex Responses request.
    ///
    /// Copilot chooses this per model, so response handlers must consume this
    /// value rather than re-deriving the format from static provider metadata.
    pub codex_upstream_format: Option<CodexUpstreamFormat>,
    /// 实际发往上游的模型名（路由接管/模型映射后的真值）。
    ///
    /// usage 归因不能依赖 ctx.request_model（映射前的客户端别名）：上游响应
    /// 缺失 model 或回显别名时，接管流量会被记成 claude-* 并按其定价计费。
    pub outbound_model: Option<String>,
    /// 活跃连接 RAII guard：随响应一起流转到 response_processor / handle_claude_transform，
    /// 最终被 move 进流式 body future（或非流式响应作用域），覆盖整个响应生命周期。
    pub(crate) connection_guard: Option<ActiveConnectionGuard>,
}

pub struct ForwardError {
    pub error: ProxyError,
    pub provider: Option<SharedProvider>,
    /// 最后一次尝试发往上游的模型名（映射之后）。错误行记这个，不记客户端别名：
    /// 路由下 Desktop 的 claude-haiku-* 实际发的是映射后的模型，记别名会被当成调错了模型。
    pub outbound_model: Option<String>,
}

/// 活跃连接 RAII guard
///
/// 构造 +1、Drop -1，都直接落在共享的原子计数上。`Drop` 不能 await，所以计数
/// 必须是同步可写的：旧实现在 Drop 里 `handle.spawn` 一个异步任务去做减量，
/// 每个连接结束都白付一次任务调度加一次 `RwLock::write`，而计数本身只需要一条
/// `fetch_sub`。`ProxyStatus.active_connections` 那份改为读快照时镜像，给前端的
/// 序列化形状不变（见 `ProxyState::status_snapshot`）。
///
/// 设计动机：之前在 `forward_with_retry` 出口处同步 -1，但流式响应的 body 实际
/// 在 `create_logged_passthrough_stream` 内还会继续 yield 字节流，导致 UI 的
/// `active_connections` 计数过早归零。RAII guard 让"减量"由 Rust 类型系统驱动，
/// 不需要每条出口路径都手动调用。
pub(crate) struct ActiveConnectionGuard {
    active_connections: Arc<AtomicUsize>,
}

impl ActiveConnectionGuard {
    pub(crate) fn acquire(active_connections: Arc<AtomicUsize>) -> Self {
        active_connections.fetch_add(1, Ordering::Relaxed);
        Self { active_connections }
    }
}

impl Drop for ActiveConnectionGuard {
    fn drop(&mut self) {
        // 每个 guard 恰好持有一个 +1，也只 drop 一次，配平由构造保证。
        self.active_connections.fetch_sub(1, Ordering::Relaxed);
    }
}

pub struct RequestForwarder {
    /// 共享的 ProviderRouter（持有熔断器状态）
    router: Arc<ProviderRouter>,
    status: Arc<RwLock<ProxyStatus>>,
    /// 活跃连接计数（与 `status` 里的那份同源；Drop 必须同步递减，故用原子量）。
    active_connections: Arc<AtomicUsize>,
    current_providers: Arc<RwLock<std::collections::HashMap<String, (String, String)>>>,
    gemini_shadow: Arc<GeminiShadowStore>,
    codex_chat_history: Arc<CodexChatHistoryStore>,
    /// 故障转移切换管理器
    failover_manager: Arc<FailoverSwitchManager>,
    /// AppHandle，用于发射事件和更新托盘
    app_handle: Option<tauri::AppHandle>,
    /// 请求开始时的"当前供应商 ID"（用于判断是否需要同步 UI/托盘）
    current_provider_id_at_start: String,
    /// 代理会话 ID（用于 Gemini Native shadow replay）
    session_id: String,
    /// Session ID 是否由客户端提供；生成值不能作为上游缓存身份。
    session_client_provided: bool,
    /// 整流器配置
    rectifier_config: RectifierConfig,
    /// 请求创建时读取到的熔断失败阈值，记账时复用。
    circuit_failure_threshold: u32,
    /// 优化器配置
    optimizer_config: OptimizerConfig,
    /// Copilot 优化器配置
    copilot_optimizer_config: CopilotOptimizerConfig,
    /// 非流式请求超时（秒）
    non_streaming_timeout: std::time::Duration,
    /// 流式请求响应头等待超时（秒）
    streaming_first_byte_timeout: std::time::Duration,
    /// 单个客户端请求最多尝试的 provider 数。
    ///
    /// 由 `AppProxyConfig.max_retries` (UI: "请求失败时的重试次数, 0-10") 派生：
    /// `max_attempts = max_retries + 1`，所以 max_retries=0 表示仅尝试一家、
    /// max_retries=3（默认）表示最多 4 家。loop 同时受 providers.len() 自然限制。
    max_attempts: usize,
    /// Stack 模型的请求（`mode::stack`）：挂在结构体上，整流器重试再次调用 `forward()` 时照样
    /// 生效。见 [`Self::routing_state_enabled`]。
    stack_request: bool,
    /// 这个请求走故障转移队列（故障转移开着）。Codex 换家时据此按目标家改模型名，见
    /// [`super::model_mapper::map_codex_failover_model`]。
    failover_chain: bool,
    /// 最近一次 `forward()` 定下的出站模型名，失败时随 [`ForwardError`] 交给日志。
    /// 每次 `forward()` 开头清空，映射失败等没走到出站的尝试留 None（日志回落到请求名）。
    last_outbound_model: std::sync::Mutex<Option<String>>,
}

impl RequestForwarder {
    /// 预防式 media 降级：发送前对 text-only 模型把图片块替换为标记。
    ///
    /// 受 `enabled && request_media_fallback` 管辖；其中"启发式模型名单预测"
    /// 再受 `request_media_heuristic` 单独管辖（显式声明 text-only 始终生效）。
    /// 返回被替换的图片块数量（0 = 未触发或开关关闭）。
    fn apply_media_prevention(&self, body: &mut Cow<'_, Value>, provider: &Provider) -> usize {
        if !(self.rectifier_config.enabled && self.rectifier_config.request_media_fallback) {
            return 0;
        }
        // 只读先扫一遍：纯文本请求（绝大多数）没有图片块可换，保持借用不拷贝。
        if !super::media_sanitizer::contains_image_blocks(body) {
            return 0;
        }
        let replaced_media = super::media_sanitizer::replace_images_for_text_only_model(
            body.to_mut(),
            provider,
            self.rectifier_config.request_media_heuristic,
        );
        if replaced_media > 0 {
            let model = body.get("model").and_then(Value::as_str).unwrap_or("");
            log::info!(
                "[Media] Replaced {replaced_media} unsupported media block(s) for provider={}, model={}",
                provider.id,
                model
            );
        }
        replaced_media
    }

    /// 反应式 media 重试判定：上游因图片输入报错后，是否应替换图片块并对同一供应商重试一次。
    ///
    /// 受 `enabled && request_media_fallback` 管辖；不涉及 `request_media_heuristic`——
    /// 这里是上游"实测"错误后的纯恢复，不是预测，故启发式开关与它无关。
    fn media_retry_should_trigger(
        &self,
        adapter_name: &str,
        already_retried: bool,
        provider_body: &Value,
        error: &ProxyError,
    ) -> bool {
        matches!(adapter_name, "Claude" | "Codex")
            && self.rectifier_config.enabled
            && self.rectifier_config.request_media_fallback
            && !already_retried
            && super::media_sanitizer::contains_image_blocks(provider_body)
            && super::media_sanitizer::is_unsupported_image_error(error)
    }

    /// 反应式密文重试判定：原样转发的 Responses 上游拒绝了请求里别家签发的密文时，
    /// 返回要去掉哪些状态，对同一供应商重试一次。
    ///
    /// 转成 Chat / Anthropic 的上游收不到这些密文，不参与。受整流器总开关管辖。
    fn opaque_state_retry_rejection(
        &self,
        app_type: &AppType,
        provider: &Provider,
        codex_upstream_format: Option<CodexUpstreamFormat>,
        already_retried: bool,
        request: &Value,
        error: &ProxyError,
    ) -> Option<OpaqueStateRejection> {
        if already_retried
            || !matches!(app_type, AppType::Codex | AppType::GrokBuild)
            || codex_upstream_format != Some(CodexUpstreamFormat::NativeResponses)
        {
            return None;
        }
        let codex_third_party = matches!(app_type, AppType::Codex)
            && !super::providers::is_codex_official_provider(provider);
        detect_opaque_state_rejection(error, &self.rectifier_config, request, codex_third_party)
    }

    /// 压缩回合被原生 Responses 第三方以 400 拒绝时，去掉 `tool_choice` 的重试请求体。
    fn compaction_tool_choice_retry_body(
        &self,
        app_type: &AppType,
        provider: &Provider,
        codex_upstream_format: Option<CodexUpstreamFormat>,
        request: &Value,
        error: &ProxyError,
    ) -> Option<Value> {
        if !matches!(app_type, AppType::Codex)
            || codex_upstream_format != Some(CodexUpstreamFormat::NativeResponses)
            || super::providers::is_codex_official_provider(provider)
            || !matches!(error, ProxyError::UpstreamError { status: 400, .. })
        {
            return None;
        }
        super::providers::codex_compaction::compaction_retry_without_tool_choice(request)
    }

    /// 上游以 400 拒绝请求里不支持的参数时，去掉这些参数的的重试请求体。
    ///
    /// 只处理 400；解析不到被点名的参数就放弃（不猜）。模型名取自请求体，
    /// 调用方重试成功后把它和参数一起记进能力缓存。
    fn unsupported_param_retry_body(
        &self,
        request: &Value,
        error: &ProxyError,
    ) -> Option<(Value, String, Vec<String>)> {
        // 两种拒绝形态：HTTP 400，以及 HTTP 200 但流里第一条事件就是 error
        // （见 rectify_sse_error_event_start，Anthropic 系网关的典型形态）。
        let body = match error {
            ProxyError::UpstreamError {
                status: 400 | 200,
                body,
            } => body.as_deref()?,
            _ => return None,
        };
        let paths = super::unsupported_param_rectifier::parse_unsupported_params(body);
        if paths.is_empty() {
            return None;
        }
        let mut retry = request.clone();
        // 请求体里直接有的先剥（透传格式）；转换器注入的参数（`thinking` 来自
        // `reasoning.effort` 等）请求体里没有，由 `forward` 在出站体上按同一组
        // 路径剥（`rectified_params`）。这里不再以「剥掉了东西」为前提——
        // 否则 anthropic / chat 转换路径永远等不到重试。
        super::unsupported_param_rectifier::strip_request_params(&mut retry, &paths);
        let model = request.get("model").and_then(Value::as_str)?.to_string();
        Some((retry, model, paths))
    }

    /// 一个供应商本次请求的 Key 尝试顺序。
    ///
    /// 只有静态 Key 认证（Anthropic / Claude 中转 / Bearer / Google 等）参与 Key 池；
    /// 托管 OAuth（Copilot / Codex OAuth / xAI / Google OAuth）用的是动态 token，
    /// 没有「换一个 Key」这回事。返回 `None` 表示这家只有一次尝试，由 `forward`
    /// 自己决定认证材料。
    ///
    /// 没有配置 Key 池的供应商直接返回 `None`，连 `extract_auth` 都不必调用，
    /// 保证绝大多数供应商路径与引入 Key 级重试之前完全一致。
    async fn api_key_attempts(
        &self,
        app_type: &AppType,
        adapter: &dyn ProviderAdapter,
        provider: &Provider,
    ) -> Option<SmallVec<[usize; 4]>> {
        let has_pool = provider.meta.as_ref().is_some_and(|meta| {
            meta.api_keys
                .iter()
                .any(|entry| !entry.key.trim().is_empty())
        });
        if !has_pool {
            return None;
        }

        let auth = adapter.extract_auth(provider)?;
        if matches!(
            auth.strategy,
            AuthStrategy::GitHubCopilot
                | AuthStrategy::CodexOAuth
                | AuthStrategy::XaiOAuth
                | AuthStrategy::GoogleOAuth
        ) {
            return None;
        }

        self.router
            .select_api_key_indices(app_type.as_str(), provider)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        router: Arc<ProviderRouter>,
        non_streaming_timeout: u64,
        status: Arc<RwLock<ProxyStatus>>,
        active_connections: Arc<AtomicUsize>,
        current_providers: Arc<RwLock<std::collections::HashMap<String, (String, String)>>>,
        gemini_shadow: Arc<GeminiShadowStore>,
        codex_chat_history: Arc<CodexChatHistoryStore>,
        failover_manager: Arc<FailoverSwitchManager>,
        app_handle: Option<tauri::AppHandle>,
        current_provider_id_at_start: String,
        session_id: String,
        session_client_provided: bool,
        streaming_first_byte_timeout: u64,
        _streaming_idle_timeout: u64,
        rectifier_config: RectifierConfig,
        optimizer_config: OptimizerConfig,
        copilot_optimizer_config: CopilotOptimizerConfig,
        max_retries: u32,
    ) -> Self {
        // max_retries 是「失败后重试次数」语义，attempt 上限 = retries + 1。
        // saturating_add 防止 u32::MAX + 1 溢出。
        let max_attempts = (max_retries as usize).saturating_add(1);
        Self {
            router,
            status,
            active_connections,
            current_providers,
            gemini_shadow,
            codex_chat_history,
            failover_manager,
            app_handle,
            current_provider_id_at_start,
            session_id,
            session_client_provided,
            rectifier_config,
            circuit_failure_threshold: 4,
            optimizer_config,
            copilot_optimizer_config,
            non_streaming_timeout: std::time::Duration::from_secs(non_streaming_timeout),
            streaming_first_byte_timeout: std::time::Duration::from_secs(
                streaming_first_byte_timeout,
            ),
            max_attempts,
            stack_request: false,
            failover_chain: false,
            last_outbound_model: std::sync::Mutex::new(None),
        }
    }

    fn set_last_outbound_model(&self, model: Option<String>) {
        if let Ok(mut slot) = self.last_outbound_model.lock() {
            *slot = model;
        }
    }

    fn last_outbound_model(&self) -> Option<String> {
        self.last_outbound_model
            .lock()
            .ok()
            .and_then(|slot| slot.clone())
    }

    /// 标记为 Stack 模型的请求。
    pub fn stack_request(mut self, stack_request: bool) -> Self {
        self.stack_request = stack_request;
        self
    }

    /// 使用请求创建时的熔断阈值，避免结果记账再读一次配置。
    pub fn with_circuit_failure_threshold(mut self, threshold: u32) -> Self {
        self.circuit_failure_threshold = threshold;
        self
    }

    /// 这个请求走故障转移队列（故障转移开着）。
    pub fn failover_chain(mut self, failover_chain: bool) -> Self {
        self.failover_chain = failover_chain;
        self
    }

    /// 故障转移换家时按目标家改 Codex 模型名，只对对话请求。图片、搜索这些独立接口带的是
    /// 图片 / 搜索模型，不在目标家的文本模型目录里，换成它的文本模型就发错了。
    fn maps_codex_failover_model(&self, app_type: &AppType, endpoint: &str) -> bool {
        self.failover_chain
            && matches!(app_type, AppType::Codex)
            && CodexStandaloneEndpoint::from_effective_endpoint(endpoint).is_none()
    }

    /// 这个请求读写路由状态吗：熔断器（许可、结果、健康度）、「正在使用」、代理统计
    /// （总数、成功率、故障转移次数、最近错误、活跃连接）、故障转移切换。Stack 模型的请求
    /// 和普通路由完全分开，一律不碰；只有用量日志照常按供应商记。以后新增路由状态也先问它。
    fn routing_state_enabled(&self) -> bool {
        !self.stack_request
    }

    /// 请求体里的模型名已经是这家的上游名（Stack 模型的请求）：不按角色映射、不换成行里配置
    /// 的模型。以后新增模型改写也先问它。
    fn keeps_resolved_model(&self) -> bool {
        self.stack_request
    }

    async fn record_success_result(
        &self,
        provider_id: &str,
        app_type: &str,
        used_half_open_permit: bool,
    ) {
        // 名额本身由这次尝试的 HalfOpenPermitGuard 归还，这里只记结果，所以传 false。
        if used_half_open_permit {
            if let Err(e) = self
                .router
                .record_result_with_threshold(
                    provider_id,
                    app_type,
                    false,
                    true,
                    None,
                    self.circuit_failure_threshold,
                )
                .await
            {
                log::warn!(
                    "[{app_type}] 记录 Provider 成功结果失败: provider_id={provider_id}, error={e}"
                );
            }
            return;
        }

        let router = self.router.clone();
        let provider_id = provider_id.to_string();
        let app_type = app_type.to_string();
        let threshold = self.circuit_failure_threshold;
        tokio::spawn(async move {
            if let Err(e) = router
                .record_result_with_threshold(&provider_id, &app_type, false, true, None, threshold)
                .await
            {
                log::warn!(
                    "[{app_type}] 异步记录 Provider 成功结果失败: provider_id={provider_id}, error={e}"
                );
            }
        });
    }

    /// 整流（thinking signature 或 budget）重试失败后的统一收尾。
    ///
    /// `None` 表示已记录熔断器、累积 `last_error`/`last_provider`，
    /// 调用方应 `continue` 让下一家 provider 继续故障转移；
    /// `Some(ForwardError)` 表示是客户端错误，没有 provider 能修复，
    /// 调用方应直接 `return` 把错误返回给客户端。
    ///
    /// `try_next_key` 为真表示同一供应商池内还有别的 Key 可以试：这时先不结算
    /// 这家供应商（不记熔断器/健康度、也不消耗 HalfOpen 探测名额），只累积
    /// `last_error`/`last_provider`，等池内 Key 都失败了再一次性结算。
    #[allow(clippy::too_many_arguments)]
    async fn handle_rectifier_retry_failure(
        &self,
        retry_err: ProxyError,
        provider: &SharedProvider,
        app_type_str: &str,
        rectifier_label: &str,
        try_next_key: bool,
        last_error: &mut Option<ProxyError>,
        last_provider: &mut Option<SharedProvider>,
    ) -> Option<ForwardError> {
        // Provider 错误：本家上游/网络确实出问题，下一家 provider 可能可用 → 继续故障转移。
        // 客户端错误：整流后请求仍违法，下一家也修不好 → 直接返回。
        let is_provider_error = match &retry_err {
            ProxyError::Timeout(_) | ProxyError::ForwardFailed(_) => true,
            ProxyError::UpstreamError { status, .. } => *status >= 500,
            _ => false,
        };

        if is_provider_error {
            if try_next_key {
                log::warn!(
                    "[{app_type_str}] [{}] Provider {} {rectifier_label}重试失败，改用同一个供应商的下一个 Key: {}",
                    log_key::RETRY_NEXT_KEY,
                    provider.name,
                    summarize_proxy_error(&retry_err)
                );
            } else {
                self.record_provider_failure(
                    provider,
                    app_type_str,
                    &retry_err,
                    format!(
                        "Provider {} {rectifier_label}重试失败: {}",
                        provider.name, retry_err
                    ),
                )
                .await;
            }
            *last_error = Some(retry_err);
            *last_provider = Some(Arc::clone(provider));
            return None;
        }

        Some(self.finish_neutral_failure(retry_err, provider).await)
    }

    // 一次尝试和一个客户端请求的收尾记账（熔断器、「正在使用」、代理统计、故障转移切换）
    // 都经下面这几个方法，不在重试循环里直接写。

    /// 开始尝试一家：界面上显示正在尝试哪家。
    async fn note_attempt(&self, provider: &Provider) {
        if !self.routing_state_enabled() {
            return;
        }
        {
            let status = self.status.read().await;
            if status.current_provider.as_deref() == Some(provider.name.as_str())
                && status.current_provider_id.as_deref() == Some(provider.id.as_str())
            {
                return;
            }
        }
        let mut status = self.status.write().await;
        status.current_provider = Some(provider.name.clone());
        status.current_provider_id = Some(provider.id.clone());
    }

    /// 请求成功：记熔断器成功，更新「正在使用」和成功统计。实际用的不是请求开始时那家，
    /// 就计一次故障转移，并异步把当前供应商切过去（同步 UI/托盘）。
    async fn finish_success(
        &self,
        provider: &SharedProvider,
        app_type_str: &str,
        used_half_open_permit: bool,
        api_key_id: Option<String>,
        (response, claude_api_format, outbound_model): (
            ProxyResponse,
            Option<String>,
            Option<String>,
        ),
        codex_upstream_format: Option<CodexUpstreamFormat>,
    ) -> ForwardResult {
        let result = ForwardResult {
            response,
            provider: Arc::clone(provider),
            api_key_id,
            claude_api_format,
            codex_upstream_format,
            outbound_model,
            connection_guard: None,
        };
        if !self.routing_state_enabled() {
            return result;
        }

        // 普通闭合熔断状态异步记录，避免阻塞流式首包返回；
        // HalfOpen 探测仍同步等待，保证熔断状态在归还名额前就转回 Closed。
        self.record_success_result(&provider.id, app_type_str, used_half_open_permit)
            .await;

        {
            let unchanged = {
                let current_providers = self.current_providers.read().await;
                current_providers
                    .get(app_type_str)
                    .is_some_and(|(id, name)| id == &provider.id && name == &provider.name)
            };
            if !unchanged {
                let mut current_providers = self.current_providers.write().await;
                current_providers.insert(
                    app_type_str.to_string(),
                    (provider.id.clone(), provider.name.clone()),
                );
            }
        }

        {
            let mut status = self.status.write().await;
            status.success_requests += 1;
            status.last_error = None;
            if self.current_provider_id_at_start.as_str() != provider.id.as_str() {
                status.failover_count += 1;

                let fm = self.failover_manager.clone();
                let ah = self.app_handle.clone();
                let pid = provider.id.clone();
                let pname = provider.name.clone();
                let at = app_type_str.to_string();
                tokio::spawn(async move {
                    let _ = fm.try_switch(ah.as_ref(), &at, &pid, &pname).await;
                });
            }
            refresh_success_rate(&mut status);
        }

        result
    }

    /// 这家本身出了问题（下一家可能可用）：记熔断器失败，更新最近错误。调用方接着试下一家。
    async fn record_provider_failure(
        &self,
        provider: &Provider,
        app_type_str: &str,
        error: &ProxyError,
        last_error: String,
    ) {
        if !self.routing_state_enabled() {
            return;
        }
        // 熔断内存态同步记（下一家和后续请求立刻看得到），provider_health 写库
        // detach：每次失败记账在关键路径上（试下一家之前、最终错误交回客户端
        // 之前），不该在这里等一次磁盘 IO。
        self.router
            .record_result_detached(
                &provider.id,
                app_type_str,
                false,
                false,
                Some(error.to_string()),
                self.circuit_failure_threshold,
            )
            .await;
        self.status.write().await.last_error = Some(last_error);
    }

    /// 不计入熔断的失败（客户端错误、整流后仍被拒）：不记熔断器，这个请求记为失败。
    /// 占用的 HalfOpen 名额随这次尝试的 guard 归还。
    async fn finish_neutral_failure(
        &self,
        error: ProxyError,
        provider: &SharedProvider,
    ) -> ForwardError {
        if self.routing_state_enabled() {
            self.record_failed_request(error.to_string()).await;
        }
        ForwardError {
            error,
            provider: Some(Arc::clone(provider)),
            outbound_model: self.last_outbound_model(),
        }
    }

    /// 这个客户端请求最终失败：失败数加一，记下错误，重算成功率。
    async fn record_failed_request(&self, last_error: String) {
        if !self.routing_state_enabled() {
            return;
        }
        let mut status = self.status.write().await;
        status.failed_requests += 1;
        status.last_error = Some(last_error);
        refresh_success_rate(&mut status);
    }

    /// 转发请求（带故障转移）
    ///
    /// 这是 thin wrapper：在客户端请求维度记一次 `total_requests` / 调整
    /// `active_connections` / 刷新 `last_request_at`，无论 inner 走哪条出口路径，
    /// 出口处都会把 `active_connections` 回收。Per-attempt 维度（成功/失败/熔断
    /// 等）仍由 inner 内自行更新 `success_requests` / `failed_requests`。
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub async fn forward_with_retry(
        &self,
        app_type: &AppType,
        method: http::Method,
        endpoint: &str,
        body: &Value,
        headers: axum::http::HeaderMap,
        extensions: Extensions,
        providers: &[Provider],
    ) -> Result<ForwardResult, ForwardError> {
        let shared_providers: Vec<SharedProvider> = providers
            .iter()
            .map(|provider| Arc::new(provider.clone()))
            .collect();
        self.forward_with_retry_shared(
            app_type,
            method,
            endpoint,
            Arc::new(body.clone()),
            headers,
            extensions,
            &shared_providers,
        )
        .await
    }

    /// 真实代理路径使用的共享版本：Provider 链由 RequestContext 以 Arc 共享。
    #[allow(clippy::too_many_arguments)]
    pub async fn forward_with_retry_shared(
        &self,
        app_type: &AppType,
        method: http::Method,
        endpoint: &str,
        body: impl Into<Arc<Value>>,
        headers: axum::http::HeaderMap,
        extensions: Extensions,
        providers: &[SharedProvider],
    ) -> Result<ForwardResult, ForwardError> {
        let body = body.into();
        if !self.routing_state_enabled() {
            return self
                .forward_with_retry_inner(
                    app_type, method, endpoint, body, headers, extensions, providers,
                )
                .await;
        }
        // acquire 走原子量，不再为 +1 单独拿一次 status 写锁；下面这一次是每请求
        // 唯一的 status 写入（原先是两次）。
        let guard = ActiveConnectionGuard::acquire(self.active_connections.clone());
        {
            let mut s = self.status.write().await;
            s.total_requests = s.total_requests.saturating_add(1);
            s.last_request_at = Some(chrono::Utc::now());
        }
        let result = self
            .forward_with_retry_inner(
                app_type, method, endpoint, body, headers, extensions, providers,
            )
            .await;
        // 把 guard 注入到 Ok 结果，让它随响应一起流转到 response_processor，
        // 在流式 body 的 future 内才真正 drop。
        // Err 路径：guard 在函数 scope 内随返回值落地时自动 drop。
        result.map(|mut fr| {
            fr.connection_guard = Some(guard);
            fr
        })
    }

    /// 实际转发逻辑（不包含客户端维度的入口/出口计数）
    ///
    /// # Arguments
    /// * `app_type` - 应用类型
    /// * `method` - 客户端请求的 HTTP 方法（透传给上游，支持 GET/POST 等）
    /// * `endpoint` - API 端点
    /// * `body` - 请求体
    /// * `headers` - 请求头
    /// * `providers` - 已选择的 Provider 列表（由 RequestContext 提供，避免重复调用 select_providers）
    #[allow(clippy::too_many_arguments)]
    async fn forward_with_retry_inner(
        &self,
        app_type: &AppType,
        method: http::Method,
        endpoint: &str,
        body: Arc<Value>,
        headers: axum::http::HeaderMap,
        extensions: Extensions,
        providers: &[SharedProvider],
    ) -> Result<ForwardResult, ForwardError> {
        if matches!(app_type, AppType::Claude | AppType::ClaudeDesktop)
            && super::classifier_guard::classifier_retry_loop(&body)
        {
            let error = ProxyError::InvalidRequest(
                super::classifier_guard::CLASSIFIER_LOOP_MESSAGE.to_string(),
            );
            self.record_failed_request(error.to_string()).await;
            return Err(ForwardError {
                error,
                provider: providers.first().cloned(),
                outbound_model: None,
            });
        }
        // 获取适配器
        let adapter = get_adapter(app_type).ok_or_else(|| ForwardError {
            error: ProxyError::ConfigError(format!(
                "{} does not support proxy routing",
                app_type.as_str()
            )),
            provider: None,
            outbound_model: None,
        })?;
        let app_type_str = app_type.as_str();

        if providers.is_empty() {
            return Err(ForwardError {
                error: ProxyError::NoAvailableProvider,
                provider: None,
                outbound_model: None,
            });
        }

        let mut last_error = None;
        let mut last_provider = None;
        let mut attempted_providers = 0usize;
        // 单 Provider 场景下跳过熔断器检查（故障转移关闭时）；Stack 模型的请求不碰熔断器。
        let bypass_circuit_breaker = providers.len() == 1 || !self.routing_state_enabled();

        // 依次尝试每个供应商：同一供应商内先按 Key 池顺序换 Key 重试（某个 Key 被
        // 限流/失效时不必立刻放弃这家），池内 Key 全部失败（或本来就只有单 Key）
        // 才继续供应商级故障转移。
        for provider in providers.iter() {
            // 上限检查：尊重用户在 AppProxyConfig.max_retries 上配置的「重试次数」。
            // 放在熔断器 allow 检查之前，避免在已经超限时还占用 HalfOpen 探测名额。
            if attempted_providers >= self.max_attempts {
                log::warn!(
                    "[{app_type_str}] 已达最大尝试次数上限 ({}/{}), 停止故障转移",
                    attempted_providers,
                    self.max_attempts
                );
                break;
            }

            // 发起请求前先获取熔断器放行许可（HalfOpen 会占用探测名额）
            // 单 Provider 场景下跳过此检查，避免熔断器阻塞所有请求
            // 占用了 HalfOpen 名额时，`_half_open_permit` 一直持有到这次尝试结束（continue /
            // return，或客户端断开把整个 future 丢掉），drop 时归还名额。
            let (allowed, used_half_open_permit, _half_open_permit) = if bypass_circuit_breaker {
                (true, false, None)
            } else {
                let permit = self
                    .router
                    .allow_provider_request(&provider.id, app_type_str)
                    .await;
                (permit.allowed, permit.used_half_open_permit, Some(permit))
            };

            if !allowed {
                // 熔断器没放行（Open 未到恢复时间，或 HalfOpen 的探测名额被占用）：
                // 这家没有发出请求，留一行日志，和「试了但失败」区分开。
                log::info!(
                    "[{app_type_str}] [{}] Provider {} ({}) 熔断器未放行，跳过",
                    log_fwd::CIRCUIT_SKIPPED,
                    provider.name,
                    provider.id
                );
                continue;
            }

            // PRE-SEND 优化器：普通路径只共享请求体；Bedrock 需要写入时再复制。
            let provider_body = if self.optimizer_config.enabled && is_bedrock_provider(provider) {
                let mut b = (*body).clone();
                if self.optimizer_config.thinking_optimizer {
                    super::thinking_optimizer::optimize(&mut b, &self.optimizer_config);
                }
                if self.optimizer_config.cache_injection {
                    super::cache_injector::inject(&mut b, &self.optimizer_config);
                }
                Arc::new(b)
            } else {
                Arc::clone(&body)
            };

            attempted_providers += 1;

            // 更新状态中的当前 Provider 信息（per-attempt 维度的标识）
            //
            // total_requests / last_request_at / active_connections 已由
            // forward_with_retry wrapper 在客户端请求维度统一处理，这里只刷
            // 新「正在尝试哪个 provider」的展示字段。
            self.note_attempt(provider).await;

            // 本次请求在这家供应商的 Key 尝试顺序：单 Key 供应商/托管 OAuth 只有
            // 一次（`None`，由 forward 自己取认证材料），多 Key 池按调度策略排队。
            let key_attempts: SmallVec<[Option<&str>; 4]> = match self
                .api_key_attempts(app_type, adapter.as_ref(), provider)
                .await
            {
                Some(indices) if !indices.is_empty() => {
                    let meta = provider.meta.as_ref().expect("key indices imply meta");
                    indices
                        .into_iter()
                        .map(|index| Some(meta.api_keys[index].key.as_str()))
                        .collect()
                }
                _ => smallvec![None],
            };

            for (key_index, api_key) in key_attempts.iter().enumerate() {
                // 池内还有下一个 Key 时，本 Key 失败先换 Key，不结算这家供应商。
                let has_next_key = key_attempts.len() > key_index + 1;
                let selected_api_key_id = api_key
                    .as_ref()
                    .map(|key| crate::proxy::key_pool::api_key_fingerprint(key));

                // 整流器重试标记：每个 Key 尝试独立持有，避免标记跨 Key 短路
                // —— 本 Key 整流后被 5xx/timeout 击落时，下一个 Key 仍能走整流流程。
                let mut rectifier_retried = false;
                let mut budget_rectifier_retried = false;
                let mut media_rectifier_retried = false;
                let mut opaque_rectifier_retried = false;
                let mut unsupported_param_retried = false;

                // 转发请求（每个 Key 只尝试一次，Key 级重试由错误分类驱动）
                let mut attempted_codex_upstream_format = None;
                match self
                    .forward(
                        app_type,
                        &method,
                        provider,
                        endpoint,
                        &provider_body,
                        &headers,
                        &extensions,
                        adapter.as_ref(),
                        api_key.as_deref(),
                        &[],
                        &mut attempted_codex_upstream_format,
                    )
                    .await
                {
                    Ok(forwarded) => {
                        return Ok(self
                            .finish_success(
                                provider,
                                app_type_str,
                                used_half_open_permit,
                                selected_api_key_id.clone(),
                                forwarded,
                                attempted_codex_upstream_format,
                            )
                            .await);
                    }
                    Err(mut e) => {
                        // Copilot native /v1/messages can also emit Anthropic-style
                        // signature and budget errors after model passthrough.
                        let provider_type =
                            ProviderType::from_app_type_and_config(app_type, provider);
                        let is_anthropic_provider = uses_anthropic_rectifiers(provider_type);
                        let mut signature_rectifier_non_retryable_client_error = false;

                        if self.media_retry_should_trigger(
                            adapter.name(),
                            media_rectifier_retried,
                            &provider_body,
                            &e,
                        ) {
                            let mut media_body = (*provider_body).clone();
                            let replaced_images =
                                super::media_sanitizer::replace_image_blocks_with_marker(
                                    &mut media_body,
                                );

                            if replaced_images > 0 {
                                let _ = std::mem::replace(&mut media_rectifier_retried, true);
                                let model = media_body
                                    .get("model")
                                    .and_then(Value::as_str)
                                    .unwrap_or("");
                                log::info!(
                                "[{app_type_str}] [Media] Upstream rejected image input; retrying provider={} model={} with {replaced_images} image block(s) replaced by {}",
                                provider.id,
                                model,
                                super::media_sanitizer::UNSUPPORTED_IMAGE_MARKER
                            );

                                let mut media_retry_codex_upstream_format = None;
                                match self
                                    .forward(
                                        app_type,
                                        &method,
                                        provider,
                                        endpoint,
                                        &media_body,
                                        &headers,
                                        &extensions,
                                        adapter.as_ref(),
                                        api_key.as_deref(),
                                        &[],
                                        &mut media_retry_codex_upstream_format,
                                    )
                                    .await
                                {
                                    Ok(forwarded) => {
                                        log::info!(
                                        "[{app_type_str}] [Media] Unsupported-image retry succeeded"
                                    );
                                        return Ok(self
                                            .finish_success(
                                                provider,
                                                app_type_str,
                                                used_half_open_permit,
                                                selected_api_key_id.clone(),
                                                forwarded,
                                                media_retry_codex_upstream_format,
                                            )
                                            .await);
                                    }
                                    Err(retry_err) => {
                                        log::warn!(
                                        "[{app_type_str}] [Media] Unsupported-image retry still failed: {retry_err}"
                                    );
                                        if let Some(err) = self
                                            .handle_rectifier_retry_failure(
                                                retry_err,
                                                provider,
                                                app_type_str,
                                                "media 降级",
                                                has_next_key,
                                                &mut last_error,
                                                &mut last_provider,
                                            )
                                            .await
                                        {
                                            return Err(err);
                                        }
                                        continue;
                                    }
                                }
                            }
                        }

                        // 压缩回合被原生 Responses 第三方拒绝：有的上游不收 `tool_choice: "none"`
                        // （见 `compaction_retry_without_tool_choice`），去掉它对同一家重试一次。
                        if let Some(retry_body) = self.compaction_tool_choice_retry_body(
                            app_type,
                            provider,
                            attempted_codex_upstream_format,
                            &provider_body,
                            &e,
                        ) {
                            log::info!(
                                "[{app_type_str}] [RECT-030] provider={} 拒绝了压缩请求，去掉 tool_choice 后重试一次",
                                provider.id
                            );
                            let mut retry_codex_upstream_format = None;
                            match self
                                .forward(
                                    app_type,
                                    &method,
                                    provider,
                                    endpoint,
                                    &retry_body,
                                    &headers,
                                    &extensions,
                                    adapter.as_ref(),
                                    api_key.as_deref(),
                                    &[],
                                    &mut retry_codex_upstream_format,
                                )
                                .await
                            {
                                Ok(forwarded) => {
                                    log::info!("[{app_type_str}] [RECT-031] 压缩重试成功");
                                    return Ok(self
                                        .finish_success(
                                            provider,
                                            app_type_str,
                                            used_half_open_permit,
                                            selected_api_key_id.clone(),
                                            forwarded,
                                            retry_codex_upstream_format,
                                        )
                                        .await);
                                }
                                // 重试仍失败：按这次的错误走常规分类。
                                Err(retry_err) => {
                                    log::warn!(
                                        "[{app_type_str}] [RECT-032] 压缩重试仍失败: {retry_err}"
                                    );
                                    e = retry_err;
                                }
                            }
                        }

                        // 上游以 400 拒绝请求里不支持的参数：去掉它对同一家重试一次，
                        // 成功后记进模型能力缓存——后续请求（同会话或新会话）发送前就剥掉，
                        // 不再报错打断会话。
                        if !unsupported_param_retried {
                            if let Some((retry_body, retry_model, retry_paths)) =
                                self.unsupported_param_retry_body(&provider_body, &e)
                            {
                                let _ = std::mem::replace(&mut unsupported_param_retried, true);
                                log::info!(
                                    "[{app_type_str}] [RECT-040] provider={} model={retry_model} 拒绝了不支持的参数 {retry_paths:?}，去掉后重试一次",
                                    provider.id
                                );
                                let mut retry_codex_upstream_format = None;
                                match self
                                    .forward(
                                        app_type,
                                        &method,
                                        provider,
                                        endpoint,
                                        &retry_body,
                                        &headers,
                                        &extensions,
                                        adapter.as_ref(),
                                        api_key.as_deref(),
                                        &retry_paths,
                                        &mut retry_codex_upstream_format,
                                    )
                                    .await
                                {
                                    Ok(forwarded) => {
                                        log::info!(
                                            "[{app_type_str}] [RECT-041] 去参重试成功（provider={} model={retry_model}）",
                                            provider.id
                                        );
                                        self.router
                                            .remember_unsupported_params(
                                                &provider.id,
                                                &retry_model,
                                                &retry_paths,
                                            )
                                            .await;
                                        return Ok(self
                                            .finish_success(
                                                provider,
                                                app_type_str,
                                                used_half_open_permit,
                                                selected_api_key_id.clone(),
                                                forwarded,
                                                retry_codex_upstream_format,
                                            )
                                            .await);
                                    }
                                    Err(retry_err) => {
                                        log::warn!(
                                            "[{app_type_str}] [RECT-042] 去参重试仍失败: {retry_err}"
                                        );
                                        e = retry_err;
                                    }
                                }
                            }
                        }

                        if let Some(rejection) = self.opaque_state_retry_rejection(
                            app_type,
                            provider,
                            attempted_codex_upstream_format,
                            opaque_rectifier_retried,
                            &provider_body,
                            &e,
                        ) {
                            let agent_task = carries_encrypted_agent_task(&provider_body);
                            let mut opaque_body = (*provider_body).clone();
                            let rectified = rectify_opaque_state(&mut opaque_body, rejection);
                            if !rectified.applied && agent_task {
                                log::warn!(
                                    "[{app_type_str}] [RECT-023] provider={} 解不开子 agent 任务的密文（主、子 agent 不在同一家），不重试",
                                    provider.id
                                );
                                return Err(self
                                    .finish_neutral_failure(unreadable_agent_task_error(), provider)
                                    .await);
                            }
                            if rectified.applied {
                                let _ = std::mem::replace(&mut opaque_rectifier_retried, true);
                                log::info!(
                                "[{app_type_str}] [RECT-020] 上游拒绝了请求里别家签发的状态，去掉 {} 个推理条目、{} 个加密片段、{} 个别家 id，换掉 {} 个压缩条目后对 provider={} 重试一次",
                                rectified.removed_reasoning_items,
                                rectified.replaced_encrypted_parts,
                                rectified.removed_foreign_ids,
                                rectified.replaced_compaction_items,
                                provider.id
                            );

                                let mut opaque_retry_codex_upstream_format = None;
                                match self
                                    .forward(
                                        app_type,
                                        &method,
                                        provider,
                                        endpoint,
                                        &opaque_body,
                                        &headers,
                                        &extensions,
                                        adapter.as_ref(),
                                        api_key.as_deref(),
                                        &[],
                                        &mut opaque_retry_codex_upstream_format,
                                    )
                                    .await
                                {
                                    Ok(forwarded) => {
                                        log::info!("[{app_type_str}] [RECT-021] 密文整流重试成功");
                                        return Ok(self
                                            .finish_success(
                                                provider,
                                                app_type_str,
                                                used_half_open_permit,
                                                selected_api_key_id.clone(),
                                                forwarded,
                                                opaque_retry_codex_upstream_format,
                                            )
                                            .await);
                                    }
                                    // 重试仍失败：按这次的错误走常规分类，和没整流过一样
                                    // 决定是否计入熔断、是否换下一家。
                                    Err(retry_err) => {
                                        log::warn!(
                                        "[{app_type_str}] [RECT-022] 密文整流重试仍失败: {retry_err}"
                                    );
                                        if agent_task
                                            && self
                                                .opaque_state_retry_rejection(
                                                    app_type,
                                                    provider,
                                                    opaque_retry_codex_upstream_format,
                                                    false,
                                                    &opaque_body,
                                                    &retry_err,
                                                )
                                                .is_some()
                                        {
                                            log::warn!(
                                                "[{app_type_str}] [RECT-023] provider={} 解不开子 agent 任务的密文（主、子 agent 不在同一家），不再重试",
                                                provider.id
                                            );
                                            return Err(self
                                                .finish_neutral_failure(
                                                    unreadable_agent_task_error(),
                                                    provider,
                                                )
                                                .await);
                                        }
                                        e = retry_err;
                                    }
                                }
                            }
                        }

                        if is_anthropic_provider {
                            let error_message = extract_error_message(&e);
                            if should_rectify_thinking_signature(
                                error_message.as_deref(),
                                &self.rectifier_config,
                            ) {
                                // 已经重试过：直接返回错误（不可重试客户端错误）
                                if rectifier_retried {
                                    log::warn!(
                                        "[{app_type_str}] [RECT-005] 整流器已触发过，不再重试"
                                    );
                                    // 不记录熔断器，这是客户端兼容性问题
                                    return Err(self.finish_neutral_failure(e, provider).await);
                                }

                                // 首次触发：整流请求体。保留原体，避免污染后续供应商。
                                let mut signature_body = Arc::clone(&provider_body);
                                let rectified =
                                    rectify_anthropic_request(Arc::make_mut(&mut signature_body));

                                // 整流未生效：继续尝试 budget 整流路径，避免误判后短路
                                if !rectified.applied {
                                    log::warn!(
                                    "[{app_type_str}] [RECT-006] thinking 签名整流器触发但无可整流内容，继续检查 budget；若 budget 也未命中则按客户端错误返回"
                                );
                                    signature_rectifier_non_retryable_client_error = true;
                                } else {
                                    log::info!(
                                    "[{}] [RECT-001] thinking 签名整流器触发, 移除 {} thinking blocks, {} redacted_thinking blocks, {} signature fields",
                                    app_type_str,
                                    rectified.removed_thinking_blocks,
                                    rectified.removed_redacted_thinking_blocks,
                                    rectified.removed_signature_fields
                                );

                                    // 标记已重试（当前逻辑下重试后必定 return，保留标记以备将来扩展）
                                    let _ = std::mem::replace(&mut rectifier_retried, true);

                                    // 使用同一供应商重试（不计入熔断器）
                                    let mut signature_retry_codex_upstream_format = None;
                                    match self
                                        .forward(
                                            app_type,
                                            &method,
                                            provider,
                                            endpoint,
                                            &signature_body,
                                            &headers,
                                            &extensions,
                                            adapter.as_ref(),
                                            api_key.as_deref(),
                                            &[],
                                            &mut signature_retry_codex_upstream_format,
                                        )
                                        .await
                                    {
                                        Ok(forwarded) => {
                                            log::info!("[{app_type_str}] [RECT-002] 整流重试成功");
                                            return Ok(self
                                                .finish_success(
                                                    provider,
                                                    app_type_str,
                                                    used_half_open_permit,
                                                    selected_api_key_id.clone(),
                                                    forwarded,
                                                    signature_retry_codex_upstream_format,
                                                )
                                                .await);
                                        }
                                        Err(retry_err) => {
                                            log::warn!(
                                            "[{app_type_str}] [RECT-003] 整流重试仍失败: {retry_err}"
                                        );
                                            if let Some(err) = self
                                                .handle_rectifier_retry_failure(
                                                    retry_err,
                                                    provider,
                                                    app_type_str,
                                                    "整流",
                                                    has_next_key,
                                                    &mut last_error,
                                                    &mut last_provider,
                                                )
                                                .await
                                            {
                                                return Err(err);
                                            }
                                            continue;
                                        }
                                    }
                                }
                            }
                        }

                        // 检测是否需要触发 budget 整流器（仅 Claude/ClaudeAuth 供应商）
                        if is_anthropic_provider {
                            let error_message = extract_error_message(&e);
                            if should_rectify_thinking_budget(
                                error_message.as_deref(),
                                &self.rectifier_config,
                            ) {
                                // 已经重试过：直接返回错误（不可重试客户端错误）
                                if budget_rectifier_retried {
                                    log::warn!(
                                    "[{app_type_str}] [RECT-013] budget 整流器已触发过，不再重试"
                                );
                                    return Err(self.finish_neutral_failure(e, provider).await);
                                }

                                let mut budget_body = Arc::clone(&provider_body);
                                let budget_rectified =
                                    rectify_thinking_budget(Arc::make_mut(&mut budget_body));
                                if !budget_rectified.applied {
                                    log::warn!(
                                    "[{app_type_str}] [RECT-014] budget 整流器触发但无可整流内容，不做无意义重试"
                                );
                                    return Err(self.finish_neutral_failure(e, provider).await);
                                }

                                log::info!(
                                "[{}] [RECT-010] thinking budget 整流器触发, before={:?}, after={:?}",
                                app_type_str,
                                budget_rectified.before,
                                budget_rectified.after
                            );

                                let _ = std::mem::replace(&mut budget_rectifier_retried, true);

                                // 使用同一供应商重试（不计入熔断器）
                                let mut budget_retry_codex_upstream_format = None;
                                match self
                                    .forward(
                                        app_type,
                                        &method,
                                        provider,
                                        endpoint,
                                        &budget_body,
                                        &headers,
                                        &extensions,
                                        adapter.as_ref(),
                                        api_key.as_deref(),
                                        &[],
                                        &mut budget_retry_codex_upstream_format,
                                    )
                                    .await
                                {
                                    Ok(forwarded) => {
                                        log::info!(
                                            "[{app_type_str}] [RECT-011] budget 整流重试成功"
                                        );
                                        return Ok(self
                                            .finish_success(
                                                provider,
                                                app_type_str,
                                                used_half_open_permit,
                                                selected_api_key_id.clone(),
                                                forwarded,
                                                budget_retry_codex_upstream_format,
                                            )
                                            .await);
                                    }
                                    Err(retry_err) => {
                                        log::warn!(
                                        "[{app_type_str}] [RECT-012] budget 整流重试仍失败: {retry_err}"
                                    );
                                        if let Some(err) = self
                                            .handle_rectifier_retry_failure(
                                                retry_err,
                                                provider,
                                                app_type_str,
                                                "budget 整流",
                                                has_next_key,
                                                &mut last_error,
                                                &mut last_provider,
                                            )
                                            .await
                                        {
                                            return Err(err);
                                        }
                                        continue;
                                    }
                                }
                            }
                        }

                        if signature_rectifier_non_retryable_client_error {
                            return Err(self.finish_neutral_failure(e, provider).await);
                        }

                        // 先分类错误，决定是否计入 provider 健康度
                        // NonRetryable 是客户端层错误，无论换哪家 Provider 都会被拒绝，不应污染熔断器和
                        let category = self.categorize_proxy_error(&e, provider);

                        match category {
                            ErrorCategory::Retryable => {
                                if has_next_key {
                                    // 同一个供应商还有别的 Key：先换 Key，这家是否真有
                                    // 问题等池内 Key 都失败之后再一次性结算（不记熔断器/
                                    // 健康度，也不消耗 HalfOpen 探测名额）。
                                    log::warn!(
                                    "[{app_type_str}] [{}] Provider {} 的 Key #{}/{} 失败，改用下一个 Key 重试: {}",
                                    log_key::RETRY_NEXT_KEY,
                                    provider.name,
                                    key_index + 1,
                                    key_attempts.len(),
                                    summarize_proxy_error(&e)
                                );
                                    last_error = Some(e);
                                    last_provider = Some(provider.clone());
                                    continue;
                                }

                                // 可重试，且这家没有别的 Key 可以换：真正的 provider 故障
                                // → 记录失败并更新熔断器/DB 健康度
                                self.record_provider_failure(
                                    provider,
                                    app_type_str,
                                    &e,
                                    format!("Provider {} 失败: {}", provider.name, e),
                                )
                                .await;

                                if key_attempts.len() > 1 {
                                    log::warn!(
                                    "[{app_type_str}] [{}] Provider {} 的 {} 个 Key 都失败了，转供应商级故障转移",
                                    log_key::POOL_EXHAUSTED,
                                    provider.name,
                                    key_attempts.len()
                                );
                                }

                                let (log_code, log_message) = build_retryable_failure_log(
                                    &provider.name,
                                    attempted_providers,
                                    providers.len(),
                                    &e,
                                );
                                log::warn!("[{app_type_str}] [{log_code}] {log_message}");

                                last_error = Some(e);
                                last_provider = Some(provider.clone());
                                // 继续尝试下一个供应商
                                continue;
                            }
                            ErrorCategory::NonRetryable => {
                                // 不可重试：客户端层错误或客户端断连 → 不污染健康度，仅释放 HalfOpen permit
                                return Err(self.finish_neutral_failure(e, provider).await);
                            }
                        }
                    }
                }
            }
        }

        if attempted_providers == 0 {
            // providers 列表非空，但全部被熔断器拒绝（典型：HalfOpen 探测名额被占用）
            self.record_failed_request("所有供应商暂时不可用（熔断器限制）".to_string())
                .await;
            return Err(ForwardError {
                error: ProxyError::NoAvailableProvider,
                provider: None,
                outbound_model: None,
            });
        }

        // 所有供应商都失败了
        self.record_failed_request("所有供应商都失败".to_string())
            .await;

        if let Some((log_code, log_message)) =
            build_terminal_failure_log(attempted_providers, providers.len(), last_error.as_ref())
        {
            log::warn!("[{app_type_str}] [{log_code}] {log_message}");
        }

        Err(ForwardError {
            error: last_error.unwrap_or(ProxyError::MaxRetriesExceeded),
            provider: last_provider,
            outbound_model: self.last_outbound_model(),
        })
    }

    /// 转发单个请求（使用适配器）
    ///
    /// 成功时返回 `(response, claude_api_format, outbound_model)`，其中
    /// `outbound_model` 是最终发往上游的模型名（所有映射/改写之后）。
    /// `codex_upstream_format_out` 记下这次实际选中的 Codex 上游格式：成功时交给
    /// `finish_success`，失败时给整流判断用（Copilot 按模型逐次选协议，不能从静态配置推）。
    ///
    /// `api_key_override` 是调用方（Key 级重试循环）算好的 Key；`None` 表示这一次
    /// 由本函数自己按 Key 池策略选一个（单 Key 路径的直接调用）。
    #[allow(clippy::too_many_arguments)]
    async fn forward(
        &self,
        app_type: &AppType,
        method: &http::Method,
        provider: &Provider,
        endpoint: &str,
        body: &Value,
        headers: &axum::http::HeaderMap,
        extensions: &Extensions,
        adapter: &dyn ProviderAdapter,
        api_key_override: Option<&str>,
        // 本次重试新学到的「这家这个模型不支持的参数」。作用于出站体：转换器
        // 注入的参数（`thinking` 来自 `reasoning.effort`、`max_tokens` 是默认值）
        // 在调用方的请求体里不存在，只有出站体上剥得掉。能力缓存的历史结论由
        // 本函数自己查（RECT-043），和这个参数合并后一起剥。
        rectified_params: &[String],
        codex_upstream_format_out: &mut Option<CodexUpstreamFormat>,
    ) -> Result<(ProxyResponse, Option<String>, Option<String>), ProxyError> {
        *codex_upstream_format_out = None;
        self.set_last_outbound_model(None);
        // 使用适配器提取 base_url
        let mut base_url = adapter.extract_base_url(provider)?;

        let is_full_url = provider
            .meta
            .as_ref()
            .and_then(|meta| meta.is_full_url)
            .unwrap_or(false)
            && !provider.is_codex_oauth()
            && !provider.is_xai_oauth()
            && !provider.is_github_copilot();

        // GitHub Copilot API 使用 /chat/completions（无 /v1 前缀）
        let is_copilot = is_managed_copilot_request(app_type, provider, &base_url);

        // Codex upstream conversion mode — computed early because the [1m]-suffix strip
        // below must be skipped on the Anthropic path (the marker has to survive to
        // catalog matching and to the transform's own strip+beta detection).
        let mut codex_responses_to_chat = matches!(app_type, AppType::Codex | AppType::GrokBuild)
            && super::providers::should_convert_codex_responses_to_chat(provider, endpoint);
        let mut codex_responses_to_anthropic =
            matches!(app_type, AppType::Codex | AppType::GrokBuild)
                && super::providers::should_convert_codex_responses_to_anthropic(
                    provider, endpoint,
                );
        let mut copilot_endpoint_override: Option<String> = None;
        let codex_official_auth_passthrough = matches!(app_type, AppType::Codex)
            && super::providers::is_codex_official_provider(provider);
        let codex_stack_request = self.stack_request && matches!(app_type, AppType::Codex);
        let mut quota_token = None;

        if codex_official_auth_passthrough {
            // 暂停使用官方登录时 Codex 带的是占位 Key，没有官方凭据可透传：本地拒绝并说明
            // 怎么恢复，不发往上游。
            if crate::settings::codex_proxy_without_official_login() {
                return Err(ProxyError::CodexOfficialLoginPaused);
            }
            let (expected_chatgpt_account_id, managed_session_matches) = match provider
                .meta
                .as_ref()
                .and_then(|meta| meta.managed_account_id_for("codex_oauth"))
            {
                Some(local_account_id) => {
                    let app_handle = self.app_handle.as_ref().ok_or_else(|| {
                        ProxyError::AuthError("Codex OAuth 认证不可用（无 AppHandle）".to_string())
                    })?;
                    let codex_state = app_handle.state::<CodexOAuthState>();
                    let chatgpt_account_id = codex_state
                        .0
                        .chatgpt_account_id_for_account(&local_account_id)
                        .await
                        .map_err(|error| {
                            ProxyError::AuthError(format!("Codex OAuth 账号解析失败: {error}"))
                        })?;
                    let session_matches = match codex_bearer_access_token(headers) {
                        Some(access_token) => {
                            crate::codex_config::codex_live_auth_matches_managed_request(
                                &local_account_id,
                                access_token,
                            )
                            .map_err(|error| {
                                ProxyError::AuthError(format!("Codex OAuth 会话校验失败: {error}"))
                            })?
                        }
                        None => false,
                    };
                    (Some(chatgpt_account_id), Some(session_matches))
                }
                None => (None, None),
            };
            validate_codex_official_authorization(
                headers,
                provider,
                expected_chatgpt_account_id.as_deref(),
                managed_session_matches,
            )?;
            if method == http::Method::POST {
                let token = codex_bearer_access_token(headers).ok_or_else(|| {
                    ProxyError::AuthError(
                        "Codex official route requires a bearer token".to_string(),
                    )
                })?;
                super::codex_quota_protection::admit(token, headers).await?;
                quota_token = Some(token);
            }
        }

        // 应用模型映射（独立于格式转换）
        // Claude Desktop proxy 模式必须先把 Desktop 可见的 claude-* route
        // 映射成真实上游模型名，并且未知 route 要直接报错，不能使用默认模型兜底。
        // 聚合模型的请求已经解出了上游名，不再查映射表（查不到会报错，按档位回落会发错模型）。
        // 请求体是 Arc 共享的，故障转移的每次尝试都要用它。这里的每一步都按
        // Copy-on-write 处理：模型映射没改写、[1m] 标记不存在时保持借用，
        // 原样转发的大请求体（数百 KB 很常见）全程零拷贝。
        let mut mapped_body: Cow<'_, Value> = if matches!(app_type, AppType::ClaudeDesktop) {
            Cow::Owned(if self.keeps_resolved_model() {
                crate::claude_desktop_config::normalize_upstream_request(body.clone(), provider)
            } else {
                crate::claude_desktop_config::map_proxy_request_model(body.clone(), provider)
                    .map_err(|e| ProxyError::InvalidRequest(e.to_string()))?
            })
        } else if self.keeps_resolved_model() {
            Cow::Borrowed(body)
        } else {
            // 模型映射保持零拷贝（没改写就继续借用）；Codex 故障转移换家时，在映射
            // 结果上再按目标家换模型名（见 maps_codex_failover_model）。
            let mapped = super::model_mapper::apply_model_mapping(body, provider)
                .map(Cow::Owned)
                .unwrap_or_else(|| Cow::Borrowed(body));
            if self.maps_codex_failover_model(app_type, endpoint) {
                Cow::Owned(super::model_mapper::map_codex_failover_model(
                    mapped.into_owned(),
                    provider,
                ))
            } else {
                mapped
            }
        };

        let is_copilot_codex_responses = matches!(app_type, AppType::Codex)
            && is_copilot
            && super::providers::is_codex_responses_endpoint(endpoint);
        let is_copilot_claude_body = is_copilot && !matches!(app_type, AppType::Codex);

        // The OpenCode gateway wants a stable per-conversation session id (see the
        // injection site below). It has to be derived here: `mapped_body` is moved
        // by the format conversion further down.
        //
        // Only that upstream consumes it, and only when the client neither sent
        // `x-opencode-session` nor supplied a session id, so guard the work: the
        // digest serialises the system prompt plus the first user message, which
        // routinely carry files and images and can reach megabytes. The host check
        // uses `base_url` - none of the URL-construction branches below rewrites the
        // authority (the only one that replaces the host wholesale is Copilot's
        // dynamic endpoint, which points at GitHub, never opencode.ai).
        let opencode_conversation_fingerprint = if !self.session_client_provided
            && !headers.contains_key("x-opencode-session")
            && native_session_header(headers).is_none()
        {
            let base_url_host = base_url
                .parse::<http::Uri>()
                .ok()
                .and_then(|u| u.authority().map(|a| a.to_string()));
            if is_opencode_upstream(base_url_host.as_deref()) {
                conversation_fingerprint(&mapped_body)
            } else {
                None
            }
        } else {
            None
        };

        // Grok Build exposes a stable client-side model profile in config.toml.
        // Route requests to the provider's real upstream model before applying
        // the optional Responses -> Chat/Anthropic bridge.
        if matches!(app_type, AppType::GrokBuild) {
            super::providers::apply_codex_upstream_model(provider, mapped_body.to_mut());
        }

        if is_copilot_claude_body {
            mapped_body = Cow::Owned(
                super::providers::copilot_model_map::apply_copilot_model_normalization(
                    mapped_body.into_owned(),
                ),
            );
            self.apply_copilot_live_model_resolution(provider, mapped_body.to_mut())
                .await;
            // Strip the [1M] context marker after Copilot normalization/resolve.
            // A user's mapped value (e.g. "gpt-5.6-sol[1M]") carries [1M] as a
            // Claude Code context-capability declaration that upstream APIs reject
            // as part of the model name. The preceding normalization step already
            // rewrites claude-xxx[1M] into the "-1m" dash form Copilot accepts, and
            // the strip helper only touches the "[1m]" bracket form, so "-1m"
            // variants pass through unchanged.
            super::model_mapper::strip_one_m_suffix_for_upstream_from_body(&mut mapped_body);
        } else if !is_copilot_codex_responses && !codex_responses_to_anthropic {
            // Skip on the Codex→Anthropic path: stripping [1m] here would break both the
            // model-catalog match (apply_codex_upstream_model) and the transform's own
            // strip+`context-1m` beta detection. The marker is stripped later, on the
            // final anthropic_body.
            super::model_mapper::strip_one_m_suffix_for_upstream_from_body(&mut mapped_body);
        }

        // --- Copilot 优化器：分类 + 请求体优化（在格式转换之前执行） ---
        // 注意：确定性 ID 也在此处计算，因为 mapped_body 在格式转换时会被 move
        //
        // 执行顺序（与 copilot-api 对齐）：
        //   1. 先在原始 body 上分类（保留 tool_result 语义，避免误判为 user）
        //   2. 再清洗孤立 tool_result（防止上游 API 报错）
        //   3. 再合并 tool_result + text（减少 premium 计费）
        let run_copilot_optimizer = is_copilot_claude_body && self.copilot_optimizer_config.enabled;
        let copilot_request_context = if run_copilot_optimizer {
            // 1. 在原始 body 上分类 — 必须在清洗/合并之前执行
            //    孤立 tool_result 仍保持 tool_result 类型，分类能正确识别为 agent
            let has_anthropic_beta = headers.contains_key("anthropic-beta");
            let classification = super::copilot_optimizer::classify_request(
                &mapped_body,
                has_anthropic_beta,
                self.copilot_optimizer_config.compact_detection,
                self.copilot_optimizer_config.subagent_detection,
            );

            log::debug!(
                "[Copilot] 优化器分类: initiator={}, is_warmup={}, is_compact={}, is_subagent={}",
                classification.initiator,
                classification.is_warmup,
                classification.is_compact,
                classification.is_subagent
            );

            // 2. 孤立 tool_result 清理 — 分类完成后再清洗
            //    防止上游 API 因不匹配的 tool_result 报错导致重试/重复计费
            mapped_body = Cow::Owned(super::copilot_optimizer::sanitize_orphan_tool_results(
                mapped_body.into_owned(),
            ));

            // 3. Tool result 合并 — 将 [tool_result, text] 变为 [tool_result(含text)]
            if self.copilot_optimizer_config.tool_result_merging {
                mapped_body = Cow::Owned(super::copilot_optimizer::merge_tool_results(
                    mapped_body.into_owned(),
                ));
            }

            // 3.5. 主动剥离 thinking block — Copilot 走 OpenAI 兼容端点不识别该块
            //      避免上游拒绝后由 rectifier 反应式重试（首次请求已消耗 quota）
            if self.copilot_optimizer_config.strip_thinking {
                mapped_body = Cow::Owned(super::copilot_optimizer::strip_thinking_blocks(
                    mapped_body.into_owned(),
                ));
            }

            // 4. Warmup 小模型降级
            if self.copilot_optimizer_config.warmup_downgrade && classification.is_warmup {
                log::info!(
                    "[Copilot] Warmup 请求降级到模型: {}",
                    self.copilot_optimizer_config.warmup_model
                );
                mapped_body.to_mut()["model"] =
                    serde_json::json!(&self.copilot_optimizer_config.warmup_model);
            }

            // 预计算确定性 Request ID（在 body 被 move 之前）
            // Session 提取优先级（与 session.rs extract_from_metadata 对齐）：
            //   1. metadata.user_id 中的 _session_ 后缀
            //   2. metadata.session_id（直接字段）
            //   3. raw metadata.user_id（整串 fallback）
            //   4. x-session-id header
            let metadata = body.get("metadata");
            let session_id = metadata
                .and_then(|m| m.get("user_id"))
                .and_then(|v| v.as_str())
                .and_then(super::session::parse_session_from_user_id)
                .or_else(|| {
                    metadata
                        .and_then(|m| m.get("session_id"))
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string())
                })
                .or_else(|| {
                    metadata
                        .and_then(|m| m.get("user_id"))
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string())
                })
                .or_else(|| {
                    headers
                        .get("x-session-id")
                        .and_then(|v| v.to_str().ok())
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string())
                })
                .unwrap_or_default();
            let det_request_id = if self.copilot_optimizer_config.deterministic_request_id {
                Some(super::copilot_optimizer::deterministic_request_id(
                    &mapped_body,
                    &session_id,
                ))
            } else {
                None
            };

            // 从 session ID 派生稳定的 interaction ID（同一主对话共享）
            let interaction_id =
                super::copilot_optimizer::deterministic_interaction_id(&session_id);

            Some(CopilotRequestContext {
                classification,
                request_id: det_request_id,
                interaction_id,
            })
        } else {
            self.codex_copilot_request_context(is_copilot_codex_responses, &mapped_body)
        };

        // Codex always speaks Responses to the local proxy. Resolve the final
        // Copilot model after every model rewrite, then select only a protocol
        // that the model advertises. Messages is intentionally not supported
        // on the Codex bridge. Only auto mode may fall back from Responses to Chat.
        if is_copilot_codex_responses {
            let api_format = CodexCopilotApiFormat::from_meta(provider.meta.as_ref());
            let resolved = self
                .resolve_codex_copilot_model(provider, &mapped_body, api_format)
                .await?;
            let transport = apply_codex_copilot_model(mapped_body.to_mut(), resolved, api_format)?;
            copilot_endpoint_override = Some(transport.endpoint);
            codex_responses_to_chat = matches!(transport.protocol, CopilotProtocol::Chat);
            codex_responses_to_anthropic = false;
        }

        let codex_upstream_format = if matches!(app_type, AppType::Codex | AppType::GrokBuild)
            && super::providers::is_codex_responses_endpoint(endpoint)
        {
            Some(if codex_responses_to_anthropic {
                CodexUpstreamFormat::Anthropic
            } else if codex_responses_to_chat {
                CodexUpstreamFormat::ChatCompletions
            } else {
                CodexUpstreamFormat::NativeResponses
            })
        } else {
            None
        };
        *codex_upstream_format_out = codex_upstream_format;

        // GitHub Copilot 动态 endpoint 路由
        // 从 CopilotAuthManager 获取缓存的 API endpoint（支持企业版等非默认 endpoint）
        if is_copilot && !is_full_url {
            if let Some(app_handle) = &self.app_handle {
                let copilot_state = app_handle.state::<CopilotAuthState>();
                let copilot_auth = &copilot_state.0;

                // 从 provider.meta 获取关联的 GitHub 账号 ID
                let account_id = provider
                    .meta
                    .as_ref()
                    .and_then(|m| m.managed_account_id_for("github_copilot"));

                let dynamic_endpoint = match &account_id {
                    Some(id) => copilot_auth.get_api_endpoint(id).await,
                    None => copilot_auth.get_default_api_endpoint().await,
                };

                // 只在动态 endpoint 与当前 base_url 不同时替换
                if dynamic_endpoint != base_url {
                    log::debug!(
                        "[Copilot] 使用动态 API endpoint: {} (原: {})",
                        dynamic_endpoint,
                        base_url
                    );
                    base_url = dynamic_endpoint;
                }
            }
        }
        let resolved_claude_api_format = if adapter.name() == "Claude" {
            Some(
                self.resolve_claude_api_format(provider, &mapped_body, is_copilot)
                    .await,
            )
        } else {
            None
        };
        if adapter.name() == "Claude" {
            if let Some(api_format) = resolved_claude_api_format.as_deref() {
                // 两个历史规范化只在窄形态（推理供应商的 tool 历史、DeepSeek 官方端点的
                // disabled+effort 冲突）下才改写。只读门先行：绝大多数请求保持借用零拷贝。
                // 主动剥离 thinking block：Copilot 的 OpenAI 兼容端点不识别该块，原生
                // Messages 透传保留它，因为工具循环里的 Anthropic 会校验 thinking。
                if run_copilot_optimizer
                    && self.copilot_optimizer_config.strip_thinking
                    && super::providers::claude_api_format_needs_transform(api_format)
                {
                    mapped_body = Cow::Owned(super::copilot_optimizer::strip_thinking_blocks(
                        mapped_body.into_owned(),
                    ));
                }
                if super::providers::anthropic_messages_need_normalization(
                    &mapped_body,
                    provider,
                    api_format,
                ) {
                    super::providers::normalize_anthropic_messages_for_provider(
                        mapped_body.to_mut(),
                        provider,
                        api_format,
                    );
                }
                self.apply_media_prevention(&mut mapped_body, provider);
            }
        }
        let needs_transform = match resolved_claude_api_format.as_deref() {
            Some(api_format) => super::providers::claude_api_format_needs_transform(api_format),
            None => adapter.needs_transform(provider),
        };
        // Codex → Anthropic: Claude Code emulation is off by default and only
        // enabled when the user explicitly turns it on in the UI, so requests can
        // pass a gateway's "Claude Code only" fingerprint check (User-Agent /
        // anthropic-beta / x-app / system prompt first line). Defaulting to off
        // avoids leaking the Claude Code fingerprint and identity prompt to
        // general-purpose gateways.
        let codex_impersonate_claude_code = codex_responses_to_anthropic
            && provider
                .meta
                .as_ref()
                .and_then(|meta| meta.impersonate_claude_code)
                == Some(true);
        let (effective_endpoint, passthrough_query) =
            if let Some(copilot_endpoint) = copilot_endpoint_override.as_deref() {
                rewrite_codex_endpoint_for_copilot(endpoint, copilot_endpoint)
            } else if codex_responses_to_chat {
                rewrite_codex_responses_endpoint_to_chat(endpoint)
            } else if codex_responses_to_anthropic {
                rewrite_codex_responses_endpoint_to_anthropic(endpoint)
            } else if needs_transform && adapter.name() == "Claude" {
                let api_format = resolved_claude_api_format
                    .as_deref()
                    .unwrap_or_else(|| super::providers::get_claude_api_format(provider));
                rewrite_claude_transform_endpoint(endpoint, api_format, is_copilot, &mapped_body)
            } else {
                (
                    endpoint.to_string(),
                    split_endpoint_and_query(endpoint)
                        .1
                        .map(ToString::to_string),
                )
            };

        let codex_chat_base_is_full_endpoint =
            codex_responses_to_chat && base_url_is_full_endpoint(&base_url, "/chat/completions");

        // Defensive fallback mirroring `codex_chat_base_is_full_endpoint`: if a user pastes
        // a base URL already ending in the Anthropic `/v1/messages` endpoint but leaves the
        // "full URL" switch off, treat it as a full endpoint so we don't double-append
        // `/v1/messages` (→ `.../v1/messages/v1/messages`, a non-retryable 400). Matches the
        // exact endpoint suffix, so prefixed gateways like `.../api/v1/messages` are covered.
        let codex_anthropic_base_is_full_endpoint =
            codex_responses_to_anthropic && base_url_is_full_endpoint(&base_url, "/v1/messages");

        let codex_standalone_endpoint = matches!(app_type, AppType::Codex)
            .then(|| CodexStandaloneEndpoint::from_effective_endpoint(&effective_endpoint))
            .flatten();
        let is_copilot_unversioned_endpoint = is_copilot
            && matches!(
                split_endpoint_and_query(&effective_endpoint).0,
                "/chat/completions" | "/responses" | "/responses/compact"
            );

        let url = if matches!(resolved_claude_api_format.as_deref(), Some("gemini_native")) {
            super::gemini_url::resolve_gemini_native_url(
                &base_url,
                &effective_endpoint,
                is_full_url,
            )
        } else if is_full_url {
            if let Some(endpoint) = codex_standalone_endpoint {
                rewrite_codex_standalone_full_url(
                    &base_url,
                    passthrough_query.as_deref(),
                    endpoint,
                )?
            } else {
                append_query_to_full_url(&base_url, passthrough_query.as_deref())
            }
        } else if let Some(endpoint) = codex_standalone_endpoint
            .filter(|endpoint| endpoint.base_url_is_source_endpoint(&base_url))
        {
            // Same tolerance as `codex_chat_base_is_full_endpoint` below: a base URL
            // pasted as a complete endpoint with the full-URL switch off would
            // otherwise become `.../chat/completions/images/generations`.
            rewrite_codex_standalone_full_url(&base_url, passthrough_query.as_deref(), endpoint)?
        } else if codex_chat_base_is_full_endpoint || codex_anthropic_base_is_full_endpoint {
            append_query_to_full_url(&base_url, passthrough_query.as_deref())
        } else if is_copilot_unversioned_endpoint {
            build_copilot_unversioned_url(&base_url, &effective_endpoint)
        } else {
            adapter.build_url(&base_url, &effective_endpoint)
        };

        let (listen_address, listen_port) = {
            let status = self.status.read().await;
            (status.address.clone(), status.port)
        };
        super::upstream_guard::reject_self_forwarding(&url, &listen_address, listen_port).await?;

        // 记录映射后的出站模型名（此时 mapped_body 已完成接管映射 / [1m] 剥离 /
        // Copilot 归一化）。格式转换后若 body 仍带 model 字段会在下方刷新覆盖；
        // gemini_native 等模型在 URL 中的格式则保留此处的转换前真值。
        let mut outbound_model = mapped_body
            .get("model")
            .and_then(|m| m.as_str())
            .filter(|m| !m.is_empty())
            .map(str::to_string);
        self.set_last_outbound_model(outbound_model.clone());

        // Codex→Anthropic: when the model name carries the [1m] marker, strip the
        // suffix and add the context-1m beta header.
        let mut codex_anthropic_one_m = false;

        // 转换请求体（如果需要）
        let mut request_body: Cow<'_, Value> = if codex_responses_to_chat {
            let mut mapped_body = mapped_body.into_owned();
            let explicit_prompt_cache_key = mapped_body
                .get("prompt_cache_key")
                .and_then(|value| value.as_str())
                .map(ToString::to_string);
            let restored = self
                .codex_chat_history
                .enrich_request(
                    &mut mapped_body,
                    self.session_client_provided
                        .then_some(self.session_id.as_str()),
                )
                .await;
            if restored > 0 {
                log::debug!(
                    "[Codex] Restored or enriched {restored} cached function call item(s) for Chat upstream"
                );
            }
            if !self.keeps_resolved_model() && !is_copilot_codex_responses {
                super::providers::apply_codex_chat_upstream_model(provider, &mut mapped_body);
            }
            let reasoning_config =
                super::providers::resolve_codex_chat_reasoning_config(provider, &mapped_body);
            let mut chat_body = super::providers::transform_codex_chat::responses_to_chat_completions_with_reasoning(
                mapped_body,
                reasoning_config.as_ref(),
            )?;
            let signature_fallbacks =
                super::providers::codex_chat_common::fill_gemini_thought_signature_fallback(
                    &mut chat_body,
                );
            if signature_fallbacks > 0 {
                log::debug!(
                    "[Codex] Filled {signature_fallbacks} missing Gemini thought signature(s) with the skip placeholder"
                );
            }
            super::providers::inject_codex_chat_prompt_cache_key(
                provider,
                &mut chat_body,
                explicit_prompt_cache_key.as_deref(),
                self.session_client_provided
                    .then_some(self.session_id.as_str()),
            );
            Cow::Owned(chat_body)
        } else if codex_responses_to_anthropic {
            let mut mapped_body = mapped_body.into_owned();
            if !self.keeps_resolved_model() {
                super::providers::apply_codex_upstream_model(provider, &mut mapped_body);
            }
            // Per-provider output ceiling override. Codex does not forward its
            // `model_max_output_tokens` in the request body, so honor the value
            // configured on the provider here — it takes precedence over any
            // request-supplied `max_output_tokens` and over the default below.
            // Injecting it into the body (rather than overriding after transform)
            // lets the thinking-budget clamp size its headroom against the real
            // ceiling too. Kept per-provider to avoid a global large default that
            // would 400 on low-output-ceiling gateways.
            if let Some(max_out) = provider
                .meta
                .as_ref()
                .and_then(|meta| meta.max_output_tokens)
                .filter(|v| *v > 0)
            {
                mapped_body["max_output_tokens"] = Value::from(max_out);
            }
            // Anthropic requires max_tokens; fall back to this default only when the
            // Codex request omits max_output_tokens (rare — Codex normally sends it).
            // Kept conservative so a low-output-ceiling model or relay does not hard-400
            // on the fallback (a too-high default 400s and is non-retryable); 8192 is
            // accepted by every current Claude model and virtually all gateways. The
            // transform clamps any thinking budget below this value.
            const DEFAULT_CODEX_ANTHROPIC_MAX_TOKENS: u64 = 8192;
            let mut anthropic_body =
                super::providers::transform_codex_anthropic::responses_request_to_anthropic(
                    mapped_body,
                    DEFAULT_CODEX_ANTHROPIC_MAX_TOKENS,
                )?;
            // Handle the 1M-context marker [1m]: strip the model-name suffix (the
            // gateway doesn't recognize it) and set the flag so the beta header is
            // added. apply_codex_upstream_model may have just written back a model
            // name carrying [1m] from the provider config, so strip it once more on
            // the final body here.
            if let Some(model) = anthropic_body.get("model").and_then(|v| v.as_str()) {
                let stripped = super::model_mapper::strip_one_m_suffix_for_upstream(model);
                if stripped != model {
                    codex_anthropic_one_m = true;
                    anthropic_body["model"] = Value::String(stripped.to_string());
                }
            }
            if codex_impersonate_claude_code {
                prepend_claude_code_system_prompt(&mut anthropic_body);
            }
            // Enable Anthropic prompt caching (no beta header required). Reuse the
            // configured TTL rather than silently forcing 5m on this conversion path.
            // otherwise system/tools/history are re-sent at full price every round,
            // inflating cost and first-token latency. The injector handles the
            // string→array `system` conversion and the new-breakpoint budget.
            super::cache_injector::inject(
                &mut anthropic_body,
                &codex_anthropic_cache_config(&self.optimizer_config),
            );
            Cow::Owned(anthropic_body)
        } else if needs_transform {
            if adapter.name() == "Claude" {
                let api_format = resolved_claude_api_format
                    .as_deref()
                    .unwrap_or_else(|| super::providers::get_claude_api_format(provider));
                let mut mapped_body = mapped_body.into_owned();
                // 映射后的模型名不再带「省略 thinking 仍默认开思考」的语义（5.x 的
                // Fable / Mythos / Opus / Sonnet），按客户端原模型补成显式 adaptive，
                // effort 换算才不会当成关闭。
                if matches!(
                    api_format,
                    "openai_chat" | "openai_responses" | "gemini_native"
                ) {
                    super::providers::transform::make_default_adaptive_thinking_explicit(
                        &mut mapped_body,
                        body.get("model").and_then(|m| m.as_str()),
                    );
                }
                Cow::Owned(super::providers::transform_claude_request_for_api_format(
                    mapped_body,
                    provider,
                    api_format,
                    self.session_client_provided
                        .then_some(self.session_id.as_str()),
                    Some(self.gemini_shadow.as_ref()),
                )?)
            } else {
                Cow::Owned(adapter.transform_request(mapped_body.into_owned(), provider)?)
            }
        } else {
            mapped_body
        };

        // RECT-043 出站体版：能力缓存的参数必须在这层剥。转换器注入的参数
        // （anthropic 的 `thinking` 来自 `reasoning.effort`、`max_tokens` 是默认
        // 值、chat 的 `thinking` 同理）在调用方的请求体里根本不存在，在那边剥
        // 是空转；只有出站体上剥得掉。`rectified_params` 是本次重试刚学到的，
        // 和能力缓存的历史结论合并，一起剥。
        {
            let app_tag = app_type.as_str();
            let cached = match body.get("model").and_then(Value::as_str) {
                Some(model) => {
                    self.router
                        .unsupported_params_for(&provider.id, model)
                        .await
                }
                None => Vec::new(),
            };
            let mut paths = cached;
            paths.extend_from_slice(rectified_params);
            paths.sort();
            paths.dedup();
            if !paths.is_empty()
                && super::unsupported_param_rectifier::strip_request_params(
                    request_body.to_mut(),
                    &paths,
                )
            {
                log::info!(
                    "[{app_tag}] [RECT-043] provider={} 出站体剥掉不支持的参数 {paths:?}",
                    provider.id
                );
            }
        }

        // Codex 远程压缩，以及同一线程里别家回合留下的状态（见 `codex_compaction`）。
        // Chat / Anthropic 转换在转换器里处理；这里管原样转发的两种上游：官方清掉
        // CC Switch 产出、官方一定会拒的条目，原生 Responses 第三方把压缩触发和
        // CC Switch 包装的压缩摘要改成普通消息。看不出来源的密文原样发出，被拒后由
        // `opaque_state_rectifier` 处理。请求里没有这些条目时逐字节不变。
        if matches!(app_type, AppType::Codex)
            && !codex_responses_to_chat
            && !codex_responses_to_anthropic
        {
            if codex_official_auth_passthrough {
                // 只读探测先行：纯官方线程（绝大多数请求）一个字节都不用动，
                // 借用中的请求体也就不需要拷贝。
                if super::providers::codex_compaction::official_scrub_needed(&request_body) {
                    super::providers::codex_compaction::scrub_ccswitch_state_for_official(
                        request_body.to_mut(),
                    );
                    log::debug!(
                        "[Codex] Scrubbed third-party turn state before the official upstream (provider={})",
                        provider.id
                    );
                }
            } else if super::providers::codex_compaction::prepare_native_third_party_request(
                &mut request_body,
            ) {
                log::debug!(
                    "[Codex] Rewrote compaction items for a native Responses upstream (provider={})",
                    provider.id
                );
            }

            // Kimi 会在工具调用后再吐一条空助手消息，下一轮又拒收这段历史；DeepSeek 同样
            // 拒收，别家留在同一线程里的这种排列续到它就会中（见
            // `transform_codex_responses_kimi_history`）。只对这几家上游、只在出现这种排列
            // 时改写。
            if !codex_official_auth_passthrough
                && super::providers::transform_codex_responses_kimi_history::upstream_rejects_messages_inside_tool_calls(
                    &base_url,
                )
                && super::providers::transform_codex_responses_kimi_history::repair_tool_history(
                    request_body.to_mut(),
                )
            {
                log::debug!(
                    "[Codex] Reordered assistant messages inside tool-call groups (provider={})",
                    provider.id
                );
            }
        }

        // Native Responses passthrough to xAI, directly or via Copilot/Grok.
        // One gate so rebase conflicts stay here plus the isolate file, not
        // scattered across sanitizers. Flatten namespaces first; then apply
        // xAI request rewrites (schema, agent_message, unknown models).
        if matches!(app_type, AppType::Codex | AppType::GrokBuild)
            && !codex_responses_to_chat
            && !codex_responses_to_anthropic
            && (!provider.is_github_copilot() || is_copilot_codex_responses)
        {
            // Codex Desktop 0.167 会往新会话注入不带 call_id 的 page-context
            // function_call_output，严格的第三方原生 Responses 上游整条请求 400
            // 拒绝（上游 #9328）。官方 ChatGPT 自己注入自己收，跳过不动。
            if !super::providers::is_codex_official_provider(provider)
                && super::providers::transform_codex_responses_xai_sanitize::rewrite_xai_call_id_less_tool_outputs(
                    request_body.to_mut(),
                )
            {
                log::info!(
                    "[Codex] Rewrote function_call_output items without call_id for native Responses upstream (provider={})",
                    provider.id
                );
            }
            // namespace 展平对所有原生 Responses 上游都做：严格的第三方网关
            // 同样会 422 unknown variant "namespace"。
            if super::providers::transform_codex_responses_namespace::flatten_request_namespaces(
                request_body.to_mut(),
            )? {
                log::debug!(
                    "[Codex] Flattened namespace tools for native Responses upstream (provider={})",
                    provider.id
                );
            }
            // xAI 字段整流只对 xAI / Copilot-Grok 生效：GLM 等第三方 Responses
            // 网关不该被换上 xAI 专属的 schema 规范化。
            if super::providers::provider_needs_responses_namespace_flatten(
                provider,
                request_body.get("model").and_then(Value::as_str),
            ) {
                // Stack 请求只做字段兼容。
                let upstream_model = if self.keeps_resolved_model() {
                    None
                } else {
                    super::providers::codex_provider_upstream_model(provider)
                };
                super::providers::transform_codex_responses_xai_sanitize::apply_xai_native_responses_request_compat(
                    request_body.to_mut(),
                    &provider.id,
                    upstream_model.as_deref(),
                    &provider.settings_config,
                );
            }
        }

        // Stack 请求发往拒收托管 `web_search` 的原生 Responses 上游：去掉这个工具，和 Chat、
        // Anthropic 转换丢掉托管工具是同一件事。只对 Stack 请求、只对名单上的上游生效；
        // 路由请求和其他 Stack 请求逐字节不变。
        if codex_stack_request && !codex_responses_to_chat && !codex_responses_to_anthropic {
            let request_model = request_body
                .get("model")
                .and_then(|model| model.as_str())
                .map(str::to_string);
            if super::providers::codex_stack_upstream_rejects_web_search(
                provider,
                request_model.as_deref(),
            ) && super::providers::strip_codex_hosted_web_search(request_body.to_mut())
            {
                log::debug!(
                    "[Codex] Dropped hosted web_search for a stacked model (provider={})",
                    provider.id
                );
            }
        }

        // Moonshot / Kimi Chat Completions reject `$ref` nodes that carry sibling
        // keywords, which Codex Desktop's built-in tool schemas do (#6867). Move
        // each such `$ref` into `allOf` for that upstream only; every other
        // provider keeps byte-identical tool schemas (prompt-cache prefix intact).
        if codex_responses_to_chat
            && super::providers::transform_codex_chat_moonshot_schema::upstream_requires_ref_sibling_all_of(
                &base_url,
            )
        {
            let rewritten =
                super::providers::transform_codex_chat_moonshot_schema::wrap_ref_siblings_in_chat_tools(
                    request_body.to_mut(),
                );
            if rewritten > 0 {
                log::debug!(
                    "[Codex] Moved `$ref` siblings into allOf for {rewritten} tool schema(s) (Moonshot upstream, provider={})",
                    provider.id
                );
            }
        }

        if matches!(app_type, AppType::Codex | AppType::GrokBuild) {
            self.apply_media_prevention(&mut request_body, provider);
        }

        // 本地代理的请求体覆写先合并，再统一过滤：覆写可能带回 `_` 前缀字段，
        // 放在后面过滤一次就够，不用像以前那样过滤→覆写→再过滤走三遍整棵树。
        if !is_copilot {
            if let Some(overrides) = provider
                .meta
                .as_ref()
                .and_then(|meta| meta.local_proxy_request_overrides.as_ref())
            {
                apply_local_proxy_body_overrides(request_body.to_mut(), overrides);
            }
        }

        // 过滤私有参数（以 `_` 开头的字段），防止内部信息泄露到上游。
        // 就地摘键、不重建文档：没有私有字段（绝大多数请求）时连拷贝都不产生。
        if contains_private_params(&request_body) {
            strip_private_params(request_body.to_mut());
        }
        let filtered_body = request_body;
        // 出站 body 定稿后刷新真值（覆盖 Codex chat 上游模型覆写、转换层模型改写）
        if let Some(m) = filtered_body
            .get("model")
            .and_then(|m| m.as_str())
            .filter(|m| !m.is_empty())
        {
            outbound_model = Some(m.to_string());
        }
        self.set_last_outbound_model(outbound_model.clone());
        log_prompt_cache_trace(
            app_type,
            provider,
            &effective_endpoint,
            resolved_claude_api_format.as_deref(),
            &filtered_body,
            self.session_client_provided,
        );
        let request_is_streaming =
            is_streaming_request(&effective_endpoint, &filtered_body, headers);
        let force_identity_encoding = needs_transform
            || codex_responses_to_chat
            || codex_responses_to_anthropic
            || request_is_streaming;

        // Codex OAuth 需要注入的 ChatGPT-Account-Id（在动态 token 获取期间填充）
        let mut codex_oauth_account_id: Option<String> = None;
        let mut should_send_codex_oauth_session_headers = false;

        // 获取认证头（提前准备，用于内联替换），同时保留仅用于日志脱敏的
        // 精确认证材料。实际日志永远不输出这些值。
        let mut log_secrets: Vec<String> = Vec::new();
        let mut auth_headers = if let Some(mut auth) = adapter.extract_auth(provider) {
            // 普通 API Key 供应商可在本地路由/聚合模式下按 Key 池策略选 Key。
            // 托管 OAuth / Copilot 使用动态 token，不参与 Key 池轮换。
            //
            // 重试循环会把候选 Key 显式传入（`api_key_override`），这样同一个
            // 供应商内换 Key 重试不会重复消耗轮询游标，也只有它能对付"某个 Key
            // 被限流/失效"这类单 Key 故障。
            if !matches!(
                auth.strategy,
                AuthStrategy::GitHubCopilot
                    | AuthStrategy::CodexOAuth
                    | AuthStrategy::XaiOAuth
                    | AuthStrategy::GoogleOAuth
            ) {
                auth.api_key = match api_key_override {
                    Some(key) => key.to_string(),
                    None => match self
                        .router
                        .select_api_key_index(app_type.as_str(), provider)
                        .await
                    {
                        Some(index) => provider
                            .meta
                            .as_ref()
                            .map(|meta| meta.api_keys[index].key.clone())
                            .unwrap_or_else(|| auth.api_key.clone()),
                        None => auth.api_key.clone(),
                    },
                };
            }

            // GitHub Copilot 特殊处理：从 CopilotAuthManager 获取真实 token
            if auth.strategy == AuthStrategy::GitHubCopilot {
                if let Some(app_handle) = &self.app_handle {
                    let copilot_state = app_handle.state::<CopilotAuthState>();
                    let copilot_auth = &copilot_state.0;

                    // 从 provider.meta 获取关联的 GitHub 账号 ID（多账号支持）
                    let account_id = provider
                        .meta
                        .as_ref()
                        .and_then(|m| m.managed_account_id_for("github_copilot"));

                    // 根据账号 ID 获取对应 token（向后兼容：无账号 ID 时使用第一个账号）
                    let token_result = match &account_id {
                        Some(id) => {
                            log::debug!("[Copilot] 使用指定账号 {id} 获取 token");
                            copilot_auth.get_valid_token_for_account(id).await
                        }
                        None => {
                            log::debug!("[Copilot] 使用默认账号获取 token");
                            copilot_auth.get_valid_token().await
                        }
                    };

                    match token_result {
                        Ok(token) => {
                            auth = AuthInfo::new(token, AuthStrategy::GitHubCopilot);
                            log::debug!(
                                "[Copilot] 成功获取 Copilot token (account={})",
                                account_id.as_deref().unwrap_or("default")
                            );
                        }
                        Err(e) => {
                            log::error!(
                                "[Copilot] 获取 Copilot token 失败 (account={}): {e}",
                                account_id.as_deref().unwrap_or("default")
                            );
                            return Err(ProxyError::AuthError(format!(
                                "GitHub Copilot 认证失败: {e}"
                            )));
                        }
                    }
                } else {
                    log::error!("[Copilot] AppHandle 不可用");
                    return Err(ProxyError::AuthError(
                        "GitHub Copilot 认证不可用（无 AppHandle）".to_string(),
                    ));
                }
            }

            // Codex OAuth 特殊处理：从 CodexOAuthManager 获取真实 access_token
            if auth.strategy == AuthStrategy::CodexOAuth {
                if let Some(app_handle) = &self.app_handle {
                    let codex_state = app_handle.state::<CodexOAuthState>();
                    let codex_auth = &codex_state.0;

                    // 从 provider.meta 获取关联的 ChatGPT 账号 ID
                    let account_id = provider
                        .meta
                        .as_ref()
                        .and_then(|m| m.managed_account_id_for("codex_oauth"));

                    let resolved_account_id = match account_id {
                        Some(id) => Some(id),
                        None => codex_auth.default_account_id().await,
                    };

                    let token_result = match &resolved_account_id {
                        Some(id) => {
                            log::debug!("[CodexOAuth] 使用指定账号 {id} 获取 token");
                            codex_auth.get_valid_token_for_account(id).await
                        }
                        None => {
                            return Err(ProxyError::AuthError(
                                "Codex OAuth 认证失败: 无可用的 ChatGPT 账号".to_string(),
                            ));
                        }
                    };

                    match token_result {
                        Ok(token) => {
                            auth = AuthInfo::new(token, AuthStrategy::CodexOAuth);
                            should_send_codex_oauth_session_headers = true;
                            // 本地账号 ID 只用于绑定；请求头必须使用上游 workspace ID。
                            codex_oauth_account_id = match resolved_account_id.as_deref() {
                                Some(id) => Some(
                                    codex_auth
                                        .chatgpt_account_id_for_account(id)
                                        .await
                                        .map_err(|e| {
                                            ProxyError::AuthError(format!(
                                                "Codex OAuth 账号解析失败: {e}"
                                            ))
                                        })?,
                                ),
                                None => None,
                            };
                            log::debug!(
                                "[CodexOAuth] 成功获取 access_token (account={})",
                                codex_oauth_account_id.as_deref().unwrap_or("default")
                            );
                        }
                        Err(e) => {
                            log::error!("[CodexOAuth] 获取 access_token 失败: {e}");
                            return Err(ProxyError::AuthError(format!(
                                "Codex OAuth 认证失败: {e}"
                            )));
                        }
                    }
                } else {
                    log::error!("[CodexOAuth] AppHandle 不可用");
                    return Err(ProxyError::AuthError(
                        "Codex OAuth 认证不可用（无 AppHandle）".to_string(),
                    ));
                }
            }

            // xAI OAuth: resolve a managed account token immediately before
            // sending the request. Invalid refresh credentials are persisted as
            // requiring re-authentication by the manager.
            if auth.strategy == AuthStrategy::XaiOAuth {
                if let Some(app_handle) = &self.app_handle {
                    let xai_state = app_handle.state::<XaiOAuthState>();
                    let xai_auth = &xai_state.0;
                    let account_id = provider
                        .meta
                        .as_ref()
                        .and_then(|meta| meta.managed_account_id_for("xai_oauth"));
                    let token_result = match &account_id {
                        Some(id) => xai_auth.get_valid_token_for_account(id).await,
                        None => xai_auth.get_valid_token().await,
                    };
                    match token_result {
                        Ok(token) => {
                            auth = AuthInfo::new(token, AuthStrategy::XaiOAuth);
                            log::debug!(
                                "[XaiOAuth] 成功获取 access_token (account={})",
                                account_id.as_deref().unwrap_or("default")
                            );
                        }
                        Err(error) => {
                            log::error!("[XaiOAuth] 获取 access_token 失败: {error}");
                            return Err(ProxyError::AuthError(format!(
                                "xAI OAuth 认证失败: {error}"
                            )));
                        }
                    }
                } else {
                    return Err(ProxyError::AuthError(
                        "xAI OAuth 认证不可用（无 AppHandle）".to_string(),
                    ));
                }
            }

            for secret in std::iter::once(&auth.api_key).chain(auth.access_token.iter()) {
                if !secret.is_empty() && !log_secrets.contains(secret) {
                    log_secrets.push(secret.clone());
                }
            }

            adapter.get_auth_headers(&auth)?
        } else {
            Vec::new()
        };

        let codex_oauth_session_headers =
            if should_send_codex_oauth_session_headers && self.session_client_provided {
                build_codex_oauth_session_headers(&self.session_id)
            } else {
                Vec::new()
            };

        // 自定义 User-Agent：与 stream_check / model_fetch 共用 parse_custom_user_agent，
        // 运行时静默忽略非法值（前端在输入处给非阻断提示，不在保存时阻断）。
        // Copilot 指纹 UA 不可覆盖。
        let custom_user_agent = if is_copilot {
            None
        } else {
            provider
                .meta
                .as_ref()
                .and_then(|meta| meta.custom_user_agent_header().ok().flatten())
        };
        // Codex→Anthropic emulation: when there is no custom UA, override Codex's
        // codex_cli_rs UA with the Claude Code UA.
        let custom_user_agent = if custom_user_agent.is_none() && codex_impersonate_claude_code {
            Some(http::HeaderValue::from_static(CLAUDE_CODE_USER_AGENT))
        } else {
            custom_user_agent
        };

        // --- Copilot 优化器：动态 header 注入 ---
        if let Some(context) = copilot_request_context {
            context.apply_headers(
                &mut auth_headers,
                self.copilot_optimizer_config.request_classification,
            )?;
        }

        // Copilot 指纹头名（由 get_auth_headers 注入，需在原始头中去重）
        let copilot_fingerprint_headers: &[&str] = if is_copilot {
            &[
                "user-agent",
                "editor-version",
                "editor-plugin-version",
                "copilot-integration-id",
                "x-github-api-version",
                "openai-intent",
                // 新增 headers
                "x-initiator",
                "x-interaction-type",
                "x-interaction-id",
                "x-vscode-user-agent-library-version",
                "x-request-id",
                "x-agent-task-id",
            ]
        } else {
            &[]
        };

        // 预计算上游 host 值（用于在原位替换 host header）
        let upstream_host = url
            .parse::<http::Uri>()
            .ok()
            .and_then(|u| u.authority().map(|a| a.to_string()));

        // OpenCode Go places two hard requirements on clients
        // (<https://opencode.ai/docs/go/>); satisfy both, only for its upstream.
        //
        // 1) User-Agent: a client that identifies itself is passed through
        //    untouched (Claude Code, Codex, ...). Only a request that carries no
        //    UA at all gets this fallback - the gateway sits behind Cloudflare and
        //    rejects unidentified clients (403 Access denied; a UA clears it).
        //    The fallback is therefore applied after the header loop, next to the
        //    custom UA. An explicitly configured custom UA still wins.
        let opencode_fallback_user_agent =
            if custom_user_agent.is_none() && is_opencode_upstream(upstream_host.as_deref()) {
                Some(http::HeaderValue::from_static(concat!(
                    "cc-switch/",
                    env!("CARGO_PKG_VERSION")
                )))
            } else {
                None
            };

        // 2) Session header: without `x-opencode-session` the gateway answers
        //    400 MissingSessionID. Pass it through when the client already sends
        //    one, otherwise send the most stable identity we have
        //    (`pick_opencode_session`) - a native client header, the client's
        //    session id, the conversation fingerprint, or the generated id.
        if is_opencode_upstream(upstream_host.as_deref())
            && !headers.contains_key("x-opencode-session")
        {
            let client_id = self
                .session_client_provided
                .then_some(self.session_id.as_str());
            if let Some(session) = pick_opencode_session(
                native_session_header(headers),
                client_id,
                opencode_conversation_fingerprint.as_deref(),
                &self.session_id,
            ) {
                if let Ok(value) = http::HeaderValue::from_str(&session) {
                    auth_headers.push((http::HeaderName::from_static("x-opencode-session"), value));
                }
            }
        }

        let should_send_anthropic_headers = adapter.name() == "Claude"
            && matches!(resolved_claude_api_format.as_deref(), Some("anthropic"));

        // 预计算 anthropic-beta 值（仅 Claude）
        let anthropic_beta_value = if should_send_anthropic_headers {
            const CLAUDE_CODE_BETA: &str = "claude-code-20250219";
            Some(if let Some(beta) = headers.get("anthropic-beta") {
                if let Ok(beta_str) = beta.to_str() {
                    if beta_str.contains(CLAUDE_CODE_BETA) {
                        beta_str.to_string()
                    } else {
                        format!("{CLAUDE_CODE_BETA},{beta_str}")
                    }
                } else {
                    CLAUDE_CODE_BETA.to_string()
                }
            } else {
                CLAUDE_CODE_BETA.to_string()
            })
        } else if codex_impersonate_claude_code || codex_anthropic_one_m {
            // Codex→Anthropic: emulation injects the claude-code marker; a [1m]
            // model injects the context-1m marker.
            let mut betas: Vec<&str> = Vec::with_capacity(2);
            if codex_impersonate_claude_code {
                betas.push("claude-code-20250219");
            }
            if codex_anthropic_one_m {
                betas.push("context-1m-2025-08-07");
            }
            Some(betas.join(","))
        } else {
            None
        };

        // ============================================================
        // 构建有序 HeaderMap — 内联替换，保持客户端原始顺序
        // ============================================================
        let mut ordered_headers = http::HeaderMap::with_capacity(headers.len());
        let mut saw_auth = false;
        let mut saw_accept_encoding = false;
        let mut saw_accept = false;
        let mut saw_user_agent = false;
        let mut saw_anthropic_beta = false;
        let mut saw_anthropic_version = false;

        for (key, value) in headers {
            let key_str = key.as_str();

            // --- host — 原位替换为上游 host（保持客户端原始位置） ---
            if key_str.eq_ignore_ascii_case("host") {
                if let Some(ref host_val) = upstream_host {
                    if let Ok(hv) = http::HeaderValue::from_str(host_val) {
                        ordered_headers.append(key.clone(), hv);
                    }
                }
                continue;
            }

            // --- 连接 / 追踪 / CDN 类 — 无条件跳过 ---
            if matches!(
                key_str,
                "content-length"
                    | "transfer-encoding"
                    | "x-forwarded-host"
                    | "x-forwarded-port"
                    | "x-forwarded-proto"
                    | "forwarded"
                    | "cf-connecting-ip"
                    | "cf-ipcountry"
                    | "cf-ray"
                    | "cf-visitor"
                    | "true-client-ip"
                    | "fastly-client-ip"
                    | "x-azure-clientip"
                    | "x-azure-fdid"
                    | "x-azure-ref"
                    | "akamai-origin-hop"
                    | "x-akamai-config-log-detail"
                    | "x-request-id"
                    | "x-correlation-id"
                    | "x-trace-id"
                    | "x-amzn-trace-id"
                    | "x-b3-traceid"
                    | "x-b3-spanid"
                    | "x-b3-parentspanid"
                    | "x-b3-sampled"
                    | "traceparent"
                    | "tracestate"
            ) {
                continue;
            }

            // --- 认证类 — 用 adapter 提供的认证头替换（在原始位置） ---
            if key_str.eq_ignore_ascii_case("authorization")
                || key_str.eq_ignore_ascii_case("x-api-key")
                || key_str.eq_ignore_ascii_case("x-goog-api-key")
            {
                // Codex official account cards deliberately keep credentials
                // out of provider storage. `requires_openai_auth = true` makes
                // Codex send the active ChatGPT authorization, which must reach
                // the official upstream unchanged. Other credential headers
                // are still discarded.
                if codex_official_auth_passthrough && key_str.eq_ignore_ascii_case("authorization")
                {
                    saw_auth = true;
                    ordered_headers.append(key.clone(), value.clone());
                    continue;
                }
                if !saw_auth {
                    saw_auth = true;
                    for (ah_name, ah_value) in &auth_headers {
                        ordered_headers.append(ah_name.clone(), ah_value.clone());
                    }
                }
                continue;
            }

            // --- x-app — during Codex→Anthropic emulation, `cli` is injected uniformly below ---
            if codex_impersonate_claude_code && key_str.eq_ignore_ascii_case("x-app") {
                continue;
            }

            // --- Codex/OpenAI fingerprint headers — never leak to an Anthropic upstream ---
            // These are client/session identifiers from the incoming Codex request,
            // not Anthropic protocol headers. Forwarding them both leaks identity and
            // can defeat strict gateway fingerprint checks.
            // The full set lives in `is_codex_client_fingerprint_header` so it stays in one
            // place. (HeaderName is lowercased by the http crate, so a direct match is safe.)
            // Codex Stack 请求发往第三方时同样剥掉：官方做路由时 Codex 每个请求都带着 ChatGPT
            // 身份（`chatgpt-account-id` 等），它们只能发往官方上游。Stack 目标不会是官方账号，
            // 这里仍按上游判断，防止以后放宽。只限 Codex：Claude Code 自己的 `x-stainless-*`
            // 是 Anthropic SDK 的正常请求头，Stack 请求照常转发。
            // 路由那家是第三方时 Codex 也可能以内置 openai 身份、带着 ChatGPT 登录发请求
            // （`codex_direct` 的 `openai_route`），身份头同样只能发往官方或 ChatGPT 托管账号。
            if should_strip_codex_client_fingerprint_header(
                key_str,
                &effective_endpoint,
                codex_responses_to_anthropic,
                codex_responses_to_chat,
                codex_stack_request,
                codex_official_auth_passthrough,
                matches!(app_type, AppType::Codex) && !provider.is_codex_oauth(),
            ) {
                continue;
            }

            // --- accept — force application/json on the Codex→Anthropic path ---
            // The Codex CLI sends `Accept: text/event-stream`, whereas a native
            // Anthropic client sends `application/json` (streaming is driven by
            // the body's stream:true). Strict Anthropic gateways return 406 Not
            // Acceptable for an event-stream Accept, so normalize it here.
            if codex_responses_to_anthropic && key_str.eq_ignore_ascii_case("accept") {
                if !saw_accept {
                    saw_accept = true;
                    ordered_headers.append(
                        http::header::ACCEPT,
                        http::HeaderValue::from_static("application/json"),
                    );
                }
                continue;
            }

            // --- accept-encoding — transform / SSE 路径强制 identity，其余保留原值 ---
            if key_str.eq_ignore_ascii_case("accept-encoding") {
                if !saw_accept_encoding {
                    saw_accept_encoding = true;
                    if force_identity_encoding {
                        ordered_headers.append(
                            http::header::ACCEPT_ENCODING,
                            http::HeaderValue::from_static("identity"),
                        );
                    } else {
                        ordered_headers.append(key.clone(), value.clone());
                    }
                }
                continue;
            }

            // --- user-agent: provider-level override for local proxy routing ---
            if !is_copilot && key_str.eq_ignore_ascii_case("user-agent") {
                if !saw_user_agent {
                    saw_user_agent = true;
                    if let Some(ref ua) = custom_user_agent {
                        ordered_headers.append(http::header::USER_AGENT, ua.clone());
                    } else {
                        ordered_headers.append(key.clone(), value.clone());
                    }
                }
                continue;
            }

            // --- anthropic-beta — 用重建值替换（确保含 claude-code 标记） ---
            if key_str.eq_ignore_ascii_case("anthropic-beta") {
                if !saw_anthropic_beta {
                    saw_anthropic_beta = true;
                    if let Some(ref beta_val) = anthropic_beta_value {
                        if let Ok(hv) = http::HeaderValue::from_str(beta_val) {
                            ordered_headers.append("anthropic-beta", hv);
                        }
                    }
                }
                continue;
            }

            // --- anthropic-version — 透传客户端值 ---
            if key_str.eq_ignore_ascii_case("anthropic-version") {
                if should_send_anthropic_headers {
                    saw_anthropic_version = true;
                    ordered_headers.append(key.clone(), value.clone());
                }
                continue;
            }

            // --- Copilot 指纹头 — 跳过（由 auth_headers 提供） ---
            if copilot_fingerprint_headers
                .iter()
                .any(|h| key_str.eq_ignore_ascii_case(h))
            {
                continue;
            }

            // --- 默认：透传 ---
            ordered_headers.append(key.clone(), value.clone());
        }

        // 如果原始请求中没有认证头，在末尾追加
        if !saw_auth && !auth_headers.is_empty() {
            for (ah_name, ah_value) in &auth_headers {
                ordered_headers.append(ah_name.clone(), ah_value.clone());
            }
        }

        // transform / SSE 路径在缺失时补 identity；普通透传不主动补 accept-encoding
        if !saw_accept_encoding && force_identity_encoding {
            ordered_headers.append(
                http::header::ACCEPT_ENCODING,
                http::HeaderValue::from_static("identity"),
            );
        }

        // On the Codex→Anthropic path, add application/json when Accept is missing (matching a native Anthropic client).
        if codex_responses_to_anthropic && !saw_accept {
            ordered_headers.append(
                http::header::ACCEPT,
                http::HeaderValue::from_static("application/json"),
            );
        }

        // Codex→Anthropic emulation: inject Claude Code's x-app: cli
        if codex_impersonate_claude_code {
            ordered_headers.append("x-app", http::HeaderValue::from_static("cli"));
        }

        if !saw_user_agent {
            if let Some(ua) = custom_user_agent
                .as_ref()
                .or(opencode_fallback_user_agent.as_ref())
            {
                ordered_headers.append(http::header::USER_AGENT, ua.clone());
            }
        }

        // 如果原始请求中没有 anthropic-beta 且有值需要添加，追加
        if !saw_anthropic_beta {
            if let Some(ref beta_val) = anthropic_beta_value {
                if let Ok(hv) = http::HeaderValue::from_str(beta_val) {
                    ordered_headers.append("anthropic-beta", hv);
                }
            }
        }

        // anthropic-version: add the default only when it is missing.
        // The Codex→Anthropic path also needs this header. Note this is independent
        // of anthropic-beta: the Claude Code-specific beta is only sent when
        // impersonation is on (handled above); on the plain Codex→Anthropic path
        // (impersonation off) anthropic-version is still required but no beta is sent.
        if (should_send_anthropic_headers || codex_responses_to_anthropic) && !saw_anthropic_version
        {
            ordered_headers.append(
                "anthropic-version",
                http::HeaderValue::from_static("2023-06-01"),
            );
        }

        // Codex OAuth 反代尽量对齐官方 Codex CLI 的会话路由信号。
        // 只发送客户端提供的 session_id；生成的 UUID 每次不同，反而会破坏前缀缓存。
        for (name, value) in codex_oauth_session_headers {
            ordered_headers.insert(name, value);
        }

        // 请求体没有被任何改写触碰时，直接复用客户端发来的字节，跳过
        // JSON Value -> bytes 的二次序列化；大请求体这里是完整转发的主要 CPU 成本。
        let raw_body_extension = extensions.get::<RawRequestBody>();
        let raw_body_bytes = matches!(&filtered_body, Cow::Borrowed(_))
            .then_some(raw_body_extension)
            .flatten()
            .map(|raw| raw.bytes.clone());

        // GET/HEAD 是 idempotent/safe 方法，按 HTTP 语义不应携带 body；
        // 强行附带 JSON body 会让某些上游（如 Google Gemini 的 models.list）拒绝请求。
        let body_bytes: Bytes = if matches!(method, &http::Method::GET | &http::Method::HEAD) {
            Bytes::new()
        } else if let Some(bytes) = raw_body_bytes {
            bytes
        } else {
            serialize_request_body(
                filtered_body.as_ref(),
                raw_body_extension.map_or(0, |raw| raw.bytes.len()),
            )?
        };

        // 确保 content-type 存在
        if !ordered_headers.contains_key(http::header::CONTENT_TYPE) {
            ordered_headers.insert(
                http::header::CONTENT_TYPE,
                http::HeaderValue::from_static("application/json"),
            );
        }

        apply_local_proxy_header_overrides(
            &mut ordered_headers,
            provider
                .meta
                .as_ref()
                .and_then(|meta| meta.local_proxy_request_overrides.as_ref()),
            is_copilot,
        );

        // 托管 OAuth 的 workspace 由账号绑定决定，覆盖客户端或本地代理配置的旧值。
        if let Some(ref account_id) = codex_oauth_account_id {
            if let Ok(value) = http::HeaderValue::from_str(account_id) {
                ordered_headers.insert("chatgpt-account-id", value);
            }
        }

        reject_proxy_placeholder_for_managed_account_upstream(&url, &ordered_headers)?;

        // 日志目标 URL 的脱敏分两种情形：
        // - 有已知密钥(log_secrets 非空)：记录脱敏后的完整 URL，剥 userinfo/query
        //   并抹掉已知密钥值，保留 host+path 便于诊断 base_url 配错路径导致的 404。
        // - 无已知密钥：凭据可能整个内嵌在 path 里且无从脱敏，只记 origin，
        //   避免默认 Info 级把形如 https://gw/<KEY>/v1 的 path 完整落盘。
        let target_for_log = if log_secrets.is_empty() {
            crate::redact_url_origin_for_log(&url)
        } else {
            crate::redact_url_for_log_with_secrets(&url, &log_secrets)
        };

        // 输出请求信息日志
        let tag = adapter.name();
        let request_model = filtered_body
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("<none>");
        log::info!("[{tag}] >>> 请求目标: {target_for_log} (model={request_model})");
        log::debug!(
            "[{tag}] >>> 请求体已准备: bytes={} (content omitted)",
            body_bytes.len(),
        );

        // 序列化后释放工作副本；只有加密子任务的流式判错还需要保留请求体。
        let encrypted_agent_request = (matches!(app_type, AppType::Codex | AppType::GrokBuild)
            && !codex_responses_to_chat
            && !codex_responses_to_anthropic
            && request_is_streaming
            && self.rectifier_config.enabled
            && carries_encrypted_agent_task(&filtered_body))
        .then_some(filtered_body);

        // 确定超时
        let timeout = if self.non_streaming_timeout.is_zero() {
            std::time::Duration::from_secs(600) // 默认 600 秒
        } else {
            self.non_streaming_timeout
        };

        // 获取全局代理 URL（上游是本机地址时直连，不送进代理）
        let upstream_proxy_url = super::http_client::proxy_url_for(&url);

        // SOCKS5 代理不支持 CONNECT 隧道，需要用 reqwest
        let is_socks_proxy = upstream_proxy_url
            .as_deref()
            .map(|u| u.starts_with("socks5"))
            .unwrap_or(false);

        let preserve_exact_header_case = should_preserve_exact_header_case(
            adapter.name(),
            provider,
            resolved_claude_api_format.as_deref(),
            is_copilot,
        );

        // 发送请求
        let response = if is_socks_proxy || !preserve_exact_header_case {
            // OpenAI / Copilot / Codex 类后端不依赖原始 header 大小写；走 reqwest
            // 连接池，避免 raw TCP/TLS path 每次请求都重新握手。SOCKS5 也只能走 reqwest。
            log::debug!(
                "[Forwarder] Using pooled reqwest client (preserve_exact_header_case={preserve_exact_header_case}, socks_proxy={is_socks_proxy})"
            );
            let client = super::http_client::get_for_url(&url);
            let mut request = client.request(method.clone(), &url);
            if request_is_streaming {
                // reqwest 的 timeout 是整请求超时；流式请求交给 response_processor
                // 的首包/静默期超时控制，避免长流被总时长误杀。
                request = request.timeout(std::time::Duration::from_secs(24 * 60 * 60));
            } else if !self.non_streaming_timeout.is_zero() {
                request = request.timeout(self.non_streaming_timeout);
            }
            for (key, value) in &ordered_headers {
                request = request.header(key, value);
            }
            let send = request.body(body_bytes.clone()).send();
            let send_result = if request_is_streaming {
                let header_timeout = if self.streaming_first_byte_timeout.is_zero() {
                    timeout
                } else {
                    self.streaming_first_byte_timeout
                };
                tokio::time::timeout(header_timeout, send)
                    .await
                    .map_err(|_| {
                        ProxyError::Timeout(format!(
                            "流式响应首包超时: {}s（上游未返回响应头）",
                            header_timeout.as_secs()
                        ))
                    })?
            } else {
                send.await
            };
            let reqwest_resp = send_result.map_err(map_reqwest_send_error)?;
            let mut response = ProxyResponse::Reqwest(reqwest_resp);
            // 个别网关按客户端形态整条拒掉 reqwest 的请求（Azure 应用网关对
            // 该客户端返回 HTML 400，同一份字节经标准客户端却能通过）。这类
            // 响应对调用方没有任何有用信息，用标准客户端重发一次再放弃。
            if upstream_proxy_url.is_none() && response.is_gateway_rejection() {
                log::warn!(
                    "[{tag}] reqwest 客户端被网关拒绝（{}），同一份字节改走标准客户端重发",
                    response.status()
                );
                if let Ok(uri) = url.parse::<http::Uri>() {
                    if let Ok(retried) = super::hyper_client::send_via_hyper_util(
                        &uri,
                        method,
                        &ordered_headers,
                        extensions,
                        &body_bytes,
                        timeout,
                    )
                    .await
                    {
                        response = retried;
                    }
                }
            }
            response
        } else {
            // HTTP 代理或直连：走 hyper raw write（保持 header 大小写）
            // 如果有 HTTP 代理，hyper_client 会用 CONNECT 隧道穿过代理
            let uri: http::Uri = url.parse().map_err(|e| {
                ProxyError::ForwardFailed(format!("Invalid upstream URL ({target_for_log}): {e}"))
            })?;
            super::hyper_client::send_request(
                uri,
                &target_for_log,
                method,
                ordered_headers,
                extensions,
                body_bytes,
                timeout,
                upstream_proxy_url.as_deref(),
            )
            .await?
        };

        // Official responses (errors included) carry the account's usage headers;
        // they feed the next request's quota check without an extra lookup.
        if let Some(token) = quota_token {
            super::codex_quota_protection::observe_response(token, headers, response.headers());
        }

        // 检查响应状态
        let status = response.status();

        if status.is_success() {
            let mut response = self
                .prepare_success_response_for_failover(response, request_is_streaming)
                .await?;
            // Streaming requests normally return SSE. If a compatible gateway
            // explicitly returns JSON instead, buffer and validate it inside the retry
            // loop as well so a 2xx Anthropic error envelope can still fail over. Do
            // not buffer unknown content types: some gateways omit the SSE header.
            if codex_responses_to_anthropic && (!request_is_streaming || response.is_json()) {
                response = self
                    .validate_codex_anthropic_success_response(response)
                    .await?;
            } else if matches!(
                resolved_claude_api_format.as_deref(),
                Some("openai_responses")
            ) {
                if !request_is_streaming || response.is_json() {
                    // Claude→Responses gateways can also return a semantic failure in an
                    // HTTP 2xx Response object. Validate buffered/JSON bodies inside the
                    // retry loop so an early failure can still select another provider.
                    response = self.validate_responses_success_response(response).await?;
                } else {
                    // Delay committing the downstream stream until the upstream emits
                    // either productive output or a valid non-failure terminal event.
                    // A response.failed/error before output remains failover-safe.
                    response = self.validate_responses_stream_start(response).await?;
                }
            } else if let Some(request) = encrypted_agent_request
                .as_ref()
                .filter(|_| !response.is_json())
            {
                response = self
                    .hold_encrypted_agent_task_stream(response, request)
                    .await?;
            } else if codex_responses_to_anthropic && response.is_sse() {
                // 流式 Anthropic 兜底：上游可能 200 但第一条事件就是点名参数的
                // error 事件（400 整流器抓不到这种形态）。窥探流开头，是就剥参
                // 重试，否则原样交还。放在链尾：不抢占上面各分支的既有行为。
                response = self.rectify_sse_error_event_start(response).await?;
            }
            Ok((response, resolved_claude_api_format, outbound_model))
        } else {
            let parts = response.into_parts();
            let status_code = parts.status.as_u16();
            // 错误响应同样可能被上游压缩（content-encoding）。reqwest 未启用任何
            // 自动解压 feature，这里拿到的是原始字节；不解压的话，压缩过的错误体会
            // 在 from_utf8 处变成非 UTF-8 而被丢弃，隐藏掉上游的限流/鉴权等详情。
            let encoding = get_content_encoding(&parts.headers);
            let raw = super::hyper_client::proxy_body_bytes_with_limit(
                parts.body,
                MAX_RESPONSE_BODY_BYTES,
            )
            .await?;
            let decoded =
                decoded_body(encoding.as_deref(), &raw, MAX_RESPONSE_BODY_BYTES).into_owned();
            let body_text = String::from_utf8(decoded).ok();

            Err(ProxyError::UpstreamError {
                status: status_code,
                body: body_text,
            })
        }
    }

    /// 故障转移开启时，成功不能只看上游响应头。
    ///
    /// - 非流式：先把完整 body 读到内存，读超时/连接中断会回到 retry loop 尝试下一家。
    /// - 流式：至少等首个 chunk 到达，避免上游返回 200 后一直不吐 SSE 时被误记成功。
    async fn prepare_success_response_for_failover(
        &self,
        response: ProxyResponse,
        request_is_streaming: bool,
    ) -> Result<ProxyResponse, ProxyError> {
        if request_is_streaming {
            return self.prime_streaming_response(response).await;
        }

        if self.non_streaming_timeout.is_zero() {
            return Ok(response);
        }

        let parts = response.into_parts();
        let status = parts.status;
        let headers = parts.headers;
        let body_timeout = self.non_streaming_timeout;
        let body = tokio::time::timeout(
            body_timeout,
            super::hyper_client::proxy_body_bytes_with_limit(parts.body, MAX_RESPONSE_BODY_BYTES),
        )
        .await
        .map_err(|_| {
            ProxyError::Timeout(format!(
                "响应体读取超时: {}s（上游发完响应头后 body 未到达）",
                body_timeout.as_secs()
            ))
        })??;

        Ok(ProxyResponse::buffered(status, headers, body))
    }

    /// Some Anthropic-compatible gateways return an Anthropic error envelope with
    /// HTTP 2xx. Validate it inside the retry loop so the request can fail over to
    /// the next provider; the response transformer runs too late for that.
    async fn validate_codex_anthropic_success_response(
        &self,
        response: ProxyResponse,
    ) -> Result<ProxyResponse, ProxyError> {
        let parts = response.into_parts();
        let status = parts.status;
        let headers = parts.headers;
        let encoding = get_content_encoding(&headers);
        let raw =
            super::hyper_client::proxy_body_bytes_with_limit(parts.body, MAX_RESPONSE_BODY_BYTES)
                .await?;
        let decoded = decoded_body(encoding.as_deref(), &raw, MAX_RESPONSE_BODY_BYTES);

        if let Some(message) = codex_anthropic_error_envelope_message(&decoded) {
            return Err(ProxyError::TransformError(format!(
                "Anthropic upstream returned a 2xx error envelope: {message}"
            )));
        }

        Ok(ProxyResponse::buffered(status, headers, raw))
    }

    async fn validate_responses_success_response(
        &self,
        response: ProxyResponse,
    ) -> Result<ProxyResponse, ProxyError> {
        let parts = response.into_parts();
        let status = parts.status;
        let headers = parts.headers;
        let encoding = get_content_encoding(&headers);
        let raw =
            super::hyper_client::proxy_body_bytes_with_limit(parts.body, MAX_RESPONSE_BODY_BYTES)
                .await?;
        let decoded = decoded_body(encoding.as_deref(), &raw, MAX_RESPONSE_BODY_BYTES);

        if let Some(message) = responses_error_envelope_message(&decoded) {
            return Err(ProxyError::TransformError(format!(
                "Responses upstream returned a 2xx failure: {message}"
            )));
        }

        Ok(ProxyResponse::buffered(status, headers, raw))
    }

    async fn validate_responses_stream_start(
        &self,
        response: ProxyResponse,
    ) -> Result<ProxyResponse, ProxyError> {
        const MAX_PRIME_BYTES: usize = 256 * 1024;

        let parts = response.into_parts();
        let status = parts.status;
        let headers = parts.headers;
        let mut stream = parts.body;
        let mut replay_chunks: Vec<Bytes> = Vec::new();
        let mut parse_buffer = String::new();
        let mut utf8_tail = Utf8TailDecoder::default();

        loop {
            let next = if self.streaming_first_byte_timeout.is_zero() {
                stream.next().await
            } else {
                tokio::time::timeout(self.streaming_first_byte_timeout, stream.next())
                    .await
                    .map_err(|_| {
                        ProxyError::Timeout(format!(
                            "Responses stream produced no semantic output within {}s",
                            self.streaming_first_byte_timeout.as_secs()
                        ))
                    })?
            };

            let Some(chunk) = next else {
                if let Some(outcome) = inspect_responses_json_document(&parse_buffer) {
                    outcome?;
                    let replay = futures::stream::iter(replay_chunks.into_iter().map(Ok));
                    return Ok(ProxyResponse::streamed(status, headers, replay));
                }
                if !parse_buffer.trim().is_empty() {
                    if let Some(outcome) = inspect_responses_start_event(parse_buffer.trim()) {
                        outcome?;
                        let replay = futures::stream::iter(replay_chunks.into_iter().map(Ok));
                        return Ok(ProxyResponse::streamed(status, headers, replay));
                    }
                }
                return Err(ProxyError::ForwardFailed(
                    "Responses stream ended before producing output or a terminal event"
                        .to_string(),
                ));
            };
            let chunk = chunk.map_err(|error| {
                ProxyError::ForwardFailed(format!(
                    "Failed while validating Responses stream start: {error}"
                ))
            })?;
            utf8_tail.append(&mut parse_buffer, &chunk);
            replay_chunks.push(chunk);

            // Some compatible gateways ignore `stream:true` and return a complete
            // Responses JSON document without a JSON content-type. Recognize that
            // shape before looking for SSE delimiters; pretty-printed JSON may itself
            // contain blank lines and must stay intact.
            if let Some(outcome) = inspect_responses_json_document(&parse_buffer) {
                outcome?;
                let replay = futures::stream::iter(replay_chunks.into_iter().map(Ok)).chain(stream);
                return Ok(ProxyResponse::streamed(status, headers, replay));
            }

            while let Some((end, delim)) = crate::proxy::sse::next_sse_block_range(&parse_buffer) {
                let block = &parse_buffer[..end];
                let outcome = inspect_responses_start_event(block);
                parse_buffer.drain(..end + delim);
                if let Some(outcome) = outcome {
                    outcome?;
                    let replay =
                        futures::stream::iter(replay_chunks.into_iter().map(Ok)).chain(stream);
                    return Ok(ProxyResponse::streamed(status, headers, replay));
                }
            }

            if replay_chunks.iter().map(Bytes::len).sum::<usize>() >= MAX_PRIME_BYTES {
                log::warn!(
                    "[Claude/Responses] semantic stream priming exceeded {MAX_PRIME_BYTES} bytes; committing buffered stream"
                );
                let replay = futures::stream::iter(replay_chunks.into_iter().map(Ok)).chain(stream);
                return Ok(ProxyResponse::streamed(status, headers, replay));
            }
        }
    }

    /// 上游返回 200，但流的第一条事件就是 error 事件的形态（Anthropic 系网关
    /// 拒绝转换注入的参数时就这样回：先发一条 `event: error` 再断流，客户端
    /// 见到流内报错就中止会话，后续同会话请求全废）。HTTP 400 整流器抓不到
    /// 这种形态，这里在把流交给客户端之前先窥探第一条事件：
    ///
    /// 是 error 事件时变成 `UpstreamError { status: 200 }` 回到重试循环，由
    /// RECT-040 剥参重试（整流器认 200 形态）；解析不出参数时错误原样
    /// surfaced，客户端至少拿到完整错误而不是流内中断。不是 error 事件时，读过
    /// 的字节原样接回去，客户端看到的流和没经过这里一样。Anthropic / OpenAI
    /// 的 error 事件都出现在任何输出之前，所以只看第一条完整块就能定论。
    async fn rectify_sse_error_event_start(
        &self,
        response: ProxyResponse,
    ) -> Result<ProxyResponse, ProxyError> {
        const MAX_PRIME_BYTES: usize = 64 * 1024;

        let parts = response.into_parts();
        let status = parts.status;
        let headers = parts.headers;
        let mut stream = parts.body;
        let mut replay_chunks: Vec<Bytes> = Vec::new();
        let mut parse_buffer = String::new();
        let mut utf8_tail = Utf8TailDecoder::default();

        loop {
            let next = if self.streaming_first_byte_timeout.is_zero() {
                stream.next().await
            } else {
                tokio::time::timeout(self.streaming_first_byte_timeout, stream.next())
                    .await
                    .map_err(|_| {
                        ProxyError::Timeout(format!(
                            "SSE 流在 {}s 内没有产出第一个事件",
                            self.streaming_first_byte_timeout.as_secs()
                        ))
                    })?
            };

            let Some(chunk) = next else {
                // 流在任何完整事件前就结束了：交出手上的字节，让下游自己收尾。
                let replay = futures::stream::iter(replay_chunks.into_iter().map(Ok));
                return Ok(ProxyResponse::streamed(status, headers, replay));
            };
            let chunk = chunk.map_err(|error| {
                ProxyError::ForwardFailed(format!("窥探 SSE 流开头时读失败: {error}"))
            })?;
            utf8_tail.append(&mut parse_buffer, &chunk);
            replay_chunks.push(chunk);

            if let Some((end, _delim)) = crate::proxy::sse::next_sse_block_range(&parse_buffer) {
                let block = &parse_buffer[..end];
                if let Some(outcome) = inspect_sse_error_event(block) {
                    // error 事件：回到重试循环整流；不是 error：原样交还。
                    outcome?;
                }
                let replay = futures::stream::iter(replay_chunks.into_iter().map(Ok)).chain(stream);
                return Ok(ProxyResponse::streamed(status, headers, replay));
            }

            if replay_chunks.iter().map(Bytes::len).sum::<usize>() >= MAX_PRIME_BYTES {
                log::warn!("[SSE] 流开头窥探超过 {MAX_PRIME_BYTES} 字节仍无完整事件，按原样交还");
                let replay = futures::stream::iter(replay_chunks.into_iter().map(Ok)).chain(stream);
                return Ok(ProxyResponse::streamed(status, headers, replay));
            }
        }
    }

    /// 原样转发的 Responses 请求末尾是一条带密文的子 agent 任务时（见
    /// `opaque_state_rectifier::carries_encrypted_agent_task`），先看到第一个不是生命周期的事件
    /// 再交出流：上游解不开密文时会在流里、输出之前回 `response.failed`，这时改成上游错误，
    /// 回到重试循环里和 HTTP 报错一样整流或说明原因。其余情况（有输出、别的失败、到了截止
    /// 时间、缓冲太多）把读过的字节原样接回去，客户端看到的流和没经过这里一样。
    ///
    /// 整个缓冲阶段只有一个截止时间（不是每读一块重新计时）：上游在输出前一直发心跳时，流
    /// 也得按时交出去。解不开密文是上游一收到请求就报的错，不用等到生成，所以最多压
    /// [`MAX_HOLD`]，首包超时更短时按首包超时。
    async fn hold_encrypted_agent_task_stream(
        &self,
        response: ProxyResponse,
        request: &Value,
    ) -> Result<ProxyResponse, ProxyError> {
        const MAX_HOLD_BYTES: usize = 256 * 1024;
        const MAX_HOLD: std::time::Duration = std::time::Duration::from_secs(10);

        let window = match self.streaming_first_byte_timeout {
            timeout if timeout.is_zero() => MAX_HOLD,
            timeout => timeout.min(MAX_HOLD),
        };
        let deadline = tokio::time::Instant::now() + window;
        let parts = response.into_parts();
        let status = parts.status;
        let headers = parts.headers;
        let mut stream = parts.body;
        let mut replay_chunks: Vec<Bytes> = Vec::new();
        let mut held_bytes = 0usize;
        let mut parse_buffer = String::new();
        let mut utf8_tail = Utf8TailDecoder::default();

        'hold: loop {
            let Ok(next) = tokio::time::timeout_at(deadline, stream.next()).await else {
                break 'hold;
            };
            let Some(chunk) = next else {
                break 'hold;
            };
            // 还没有交出流，读出错和首包前出错一样回到重试循环。
            let chunk = chunk
                .map_err(|error| ProxyError::ForwardFailed(format!("读取流式响应失败: {error}")))?;
            utf8_tail.append(&mut parse_buffer, &chunk);
            held_bytes += chunk.len();
            replay_chunks.push(chunk);

            while let Some((end, delim)) = crate::proxy::sse::next_sse_block_range(&parse_buffer) {
                let block = &parse_buffer[..end];
                let verdict = encrypted_task_stream_start(block, request);
                parse_buffer.drain(..end + delim);
                match verdict {
                    StreamStart::Pending => {}
                    StreamStart::Commit => break 'hold,
                    StreamStart::Rejected(error) => return Err(error),
                }
            }
            if held_bytes > MAX_HOLD_BYTES {
                break 'hold;
            }
        }

        let replay = futures::stream::iter(replay_chunks.into_iter().map(Ok)).chain(stream);
        Ok(ProxyResponse::streamed(status, headers, replay))
    }

    async fn prime_streaming_response(
        &self,
        response: ProxyResponse,
    ) -> Result<ProxyResponse, ProxyError> {
        if self.streaming_first_byte_timeout.is_zero() {
            return Ok(response);
        }

        let parts = response.into_parts();
        let status = parts.status;
        let headers = parts.headers;
        let timeout = self.streaming_first_byte_timeout;
        let mut stream = parts.body;

        let first = tokio::time::timeout(timeout, stream.next())
            .await
            .map_err(|_| {
                ProxyError::Timeout(format!(
                    "流式响应首包超时: {}s（上游已返回响应头但未返回数据）",
                    timeout.as_secs()
                ))
            })?;

        let Some(first) = first else {
            return Err(ProxyError::ForwardFailed(
                "流式响应在首包到达前结束".to_string(),
            ));
        };

        let first =
            first.map_err(|e| ProxyError::ForwardFailed(format!("读取流式响应首包失败: {e}")))?;

        let replay = futures::stream::once(async move { Ok(first) }).chain(stream);
        Ok(ProxyResponse::streamed(status, headers, replay))
    }

    fn codex_copilot_request_context(
        &self,
        is_codex_copilot_responses: bool,
        body: &Value,
    ) -> Option<CopilotRequestContext> {
        if !is_codex_copilot_responses || !self.copilot_optimizer_config.enabled {
            return None;
        }
        Some(CopilotRequestContext {
            classification: super::copilot_optimizer::classify_responses_request(body),
            // Keep Codex request IDs per-request; only the interaction ID groups a session.
            request_id: None,
            interaction_id: self
                .session_client_provided
                .then(|| super::copilot_optimizer::deterministic_interaction_id(&self.session_id))
                .flatten(),
        })
    }

    async fn resolve_claude_api_format(
        &self,
        provider: &Provider,
        body: &Value,
        is_copilot: bool,
    ) -> String {
        if !is_copilot {
            return super::providers::get_claude_api_format(provider).to_string();
        }

        self.resolve_copilot_api_format(provider, body)
            .await
            .to_string()
    }

    async fn resolve_copilot_api_format(&self, provider: &Provider, body: &Value) -> &'static str {
        let model = body.get("model").and_then(|value| value.as_str());
        let record = match model {
            Some(model_id) => self.copilot_model_record(provider, model_id).await,
            None => None,
        };
        copilot_api_format_for_model(
            record.as_ref().map(|model| model.vendor.as_str()),
            record
                .as_ref()
                .map(|model| model.supported_endpoints.as_slice())
                .unwrap_or_default(),
        )
    }

    /// 用 Copilot live `/models` 列表确认 model ID 真实可用，找不到时按 family 降级。
    /// 命中缓存后是同步的；首次请求或 5 min 缓存过期后会触发一次 HTTP。
    async fn apply_copilot_live_model_resolution(
        &self,
        provider: &Provider,
        body: &mut serde_json::Value,
    ) {
        let Some(model_id) = body.get("model").and_then(|v| v.as_str()) else {
            return;
        };
        let model_id = model_id.to_string();

        let Some(app_handle) = &self.app_handle else {
            return;
        };
        let copilot_state = app_handle.state::<CopilotAuthState>();
        let copilot_auth = &copilot_state.0;
        let account_id = provider
            .meta
            .as_ref()
            .and_then(|m| m.managed_account_id_for("github_copilot"));

        let models_result = match account_id.as_deref() {
            Some(id) => copilot_auth.fetch_models_for_account(id).await,
            None => copilot_auth.fetch_models().await,
        };

        let models = match models_result {
            Ok(m) => m,
            Err(err) => {
                log::debug!("[Copilot] live model list unavailable, skip resolution: {err}");
                return;
            }
        };

        if let Some(resolved) =
            super::providers::copilot_model_map::resolve_against_models(&model_id, &models)
        {
            log::info!("[Copilot] live-model resolve: {model_id} → {resolved}");
            body["model"] = serde_json::Value::String(resolved);
        }
    }

    async fn resolve_codex_copilot_model(
        &self,
        provider: &Provider,
        body: &Value,
        api_format: CodexCopilotApiFormat,
    ) -> Result<Option<ResolvedCopilotModel>, ProxyError> {
        let model_id = body
            .get("model")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .ok_or_else(|| {
                ProxyError::InvalidRequest(
                    "Codex Copilot requests require a non-empty string model".to_string(),
                )
            })?;
        let app_handle = self.app_handle.as_ref().ok_or_else(|| {
            ProxyError::ConfigError(format!(
                "GitHub Copilot capabilities for model {model_id} cannot be resolved: AppHandle unavailable"
            ))
        })?;

        let copilot_state = app_handle.state::<CopilotAuthState>();
        let copilot_auth = &copilot_state.0;
        let account_id = provider
            .meta
            .as_ref()
            .and_then(|meta| meta.managed_account_id_for("github_copilot"));
        let resolved = match account_id.as_deref() {
            Some(id) => {
                copilot_auth
                    .resolve_model_for_account(id, model_id, api_format)
                    .await
            }
            None => copilot_auth.resolve_model(model_id, api_format).await,
        };

        resolved.map_err(|error| codex_copilot_lookup_error(model_id, error))
    }

    async fn copilot_model_record(
        &self,
        provider: &Provider,
        model_id: &str,
    ) -> Option<super::providers::copilot_auth::CopilotModel> {
        let Some(app_handle) = &self.app_handle else {
            log::debug!("[Copilot] AppHandle unavailable, fallback to chat/completions");
            return None;
        };

        let copilot_state = app_handle.state::<CopilotAuthState>();
        let copilot_auth = &copilot_state.0;
        let account_id = provider
            .meta
            .as_ref()
            .and_then(|m| m.managed_account_id_for("github_copilot"));

        let record_result = match account_id.as_deref() {
            Some(id) => {
                copilot_auth
                    .get_model_record_for_account(id, model_id)
                    .await
            }
            None => copilot_auth.get_model_record(model_id).await,
        };

        match record_result {
            Ok(Some(record)) => Some(record),
            Ok(None) => {
                log::debug!(
                    "[Copilot] Model vendor unavailable for {model_id}, fallback to chat/completions"
                );
                None
            }
            Err(err) => {
                log::warn!(
                    "[Copilot] Failed to resolve model vendor for {model_id}, fallback to chat/completions: {err}"
                );
                None
            }
        }
    }

    fn categorize_proxy_error(&self, error: &ProxyError, provider: &Provider) -> ErrorCategory {
        // Authentication belongs to the Codex client for an official route.
        // Every retry would reuse the selected account's inbound Authorization
        // header against another card, so no official-route error may fail over.
        if super::providers::is_codex_official_provider(provider) {
            return ErrorCategory::NonRetryable;
        }

        // xAI OAuth mirrors the same rule for token acquisition: a local
        // AuthError means the managed account needs re-login. Failing over
        // would silently move the conversation off the selected Grok account
        // and poison the provider's health state for an account-level issue.
        if provider.is_xai_oauth() && matches!(error, ProxyError::AuthError(_)) {
            return ErrorCategory::NonRetryable;
        }

        match error {
            // 网络和上游错误：都应该尝试下一个供应商
            ProxyError::Timeout(_) => ErrorCategory::Retryable,
            ProxyError::ForwardFailed(_) => ErrorCategory::Retryable,
            // 上游 HTTP 错误：按状态码分桶。
            //
            // 客户端请求自身有问题的状态码无论换哪个 provider 都会被拒绝，
            // 继续轮询只会放大错误率、污染熔断器健康度、浪费配额：
            //   400 Bad Request / 422 Unprocessable Entity   ← 请求体格式或语义错误
            //   405 Method Not Allowed / 406 Not Acceptable  ← 方法或 Accept 错误
            //   413 Payload Too Large / 414 URI Too Long     ← 客户端构造超限
            //   415 Unsupported Media Type                    ← Content-Type 错误
            //   501 Not Implemented                           ← 上游协议确实不支持
            //
            // 其他 4xx（401/403/404/408/409/429/451 等）和全部 5xx 都保留
            // Retryable —— 换一家 provider 可能持有不同的 key、配额、地域或模型映射。
            ProxyError::UpstreamError { status, .. } => match *status {
                400 | 405 | 406 | 413 | 414 | 415 | 422 | 501 => ErrorCategory::NonRetryable,
                _ => ErrorCategory::Retryable,
            },
            // Provider 级配置/转换问题：换一个 Provider 可能就能成功
            ProxyError::ConfigError(_) => ErrorCategory::Retryable,
            ProxyError::TransformError(_) => ErrorCategory::Retryable,
            ProxyError::AuthError(_) => ErrorCategory::Retryable,
            // 无可用供应商：所有供应商都试过了，无法重试
            ProxyError::NoAvailableProvider => ErrorCategory::NonRetryable,
            // 其他错误（数据库/内部错误等）：不是换供应商能解决的问题
            _ => ErrorCategory::NonRetryable,
        }
    }
}

/// Shared strict-Grok request pipeline, called only after native Responses
/// transport selection. Keep the model-aware gate alongside its transforms.
///
/// 仅测试使用：生产路径走 `forward` 里更宽的内联段落（对所有原生 Responses
/// 请求做 namespace 展平，连不需要展平的第三方网关也覆盖），这里保留上游的
/// 窄门控版本供其回归测试直接驱动。
/// 上游 #9328 起也做 call-less 输出规整，生产路径同样先走这步再展平。
#[cfg(test)]
fn apply_native_responses_request_compat(
    provider: &Provider,
    body: &mut Value,
    keep_resolved_model: bool,
) -> Result<(), ProxyError> {
    if !super::providers::is_codex_official_provider(provider)
        && super::providers::transform_codex_responses_xai_sanitize::rewrite_xai_call_id_less_tool_outputs(body)
    {
        log::info!(
            "[Codex] Rewrote function_call_output items without call_id for native Responses upstream (provider={})",
            provider.id
        );
    }
    if !super::providers::provider_needs_responses_namespace_flatten(
        provider,
        body.get("model").and_then(Value::as_str),
    ) {
        return Ok(());
    }
    if super::providers::transform_codex_responses_namespace::flatten_request_namespaces(body)? {
        log::debug!(
            "[Codex] Flattened namespace tools for native Responses upstream (provider={})",
            provider.id
        );
    }
    // Copilot already resolved against its live catalog; never replace that
    // model with the card default. Direct xAI keeps its existing fallback.
    let upstream_model = if keep_resolved_model || provider.is_github_copilot() {
        None
    } else {
        super::providers::codex_provider_upstream_model(provider)
    };
    super::providers::transform_codex_responses_xai_sanitize::apply_xai_native_responses_request_compat(
        body,
        &provider.id,
        upstream_model.as_deref(),
        &provider.settings_config,
    );
    Ok(())
}

/// 从 ProxyError 中提取错误消息
fn extract_error_message(error: &ProxyError) -> Option<String> {
    match error {
        ProxyError::UpstreamError { body, .. } => body.clone(),
        _ => Some(error.to_string()),
    }
}

/// 检测 Provider 是否为 Bedrock（通过 CLAUDE_CODE_USE_BEDROCK 环境变量判断）
fn is_bedrock_provider(provider: &Provider) -> bool {
    provider
        .settings_config
        .get("env")
        .and_then(|e| e.get("CLAUDE_CODE_USE_BEDROCK"))
        .and_then(|v| v.as_str())
        .map(|v| v == "1")
        .unwrap_or(false)
}

fn refresh_success_rate(status: &mut ProxyStatus) {
    if status.total_requests > 0 {
        status.success_rate =
            (status.success_requests as f32 / status.total_requests as f32) * 100.0;
    }
}

fn build_retryable_failure_log(
    provider_name: &str,
    attempted_providers: usize,
    total_providers: usize,
    error: &ProxyError,
) -> (&'static str, String) {
    let error_summary = summarize_proxy_error(error);

    if total_providers <= 1 {
        (
            log_fwd::SINGLE_PROVIDER_FAILED,
            format!("Provider {provider_name} 请求失败: {error_summary}"),
        )
    } else {
        (
            log_fwd::PROVIDER_FAILED_RETRY,
            format!(
                "Provider {provider_name} 失败，继续尝试下一个 ({attempted_providers}/{total_providers}): {error_summary}"
            ),
        )
    }
}

fn build_terminal_failure_log(
    attempted_providers: usize,
    total_providers: usize,
    last_error: Option<&ProxyError>,
) -> Option<(&'static str, String)> {
    if total_providers <= 1 {
        return None;
    }

    let error_summary = last_error
        .map(summarize_proxy_error)
        .unwrap_or_else(|| "未知错误".to_string());

    Some((
        log_fwd::ALL_PROVIDERS_FAILED,
        format!(
            "已尝试 {attempted_providers}/{total_providers} 个 Provider，均失败。最后错误: {error_summary}"
        ),
    ))
}

fn summarize_proxy_error(error: &ProxyError) -> String {
    match error {
        ProxyError::UpstreamError { status, body } => {
            let body_summary = body
                .as_deref()
                .map(summarize_upstream_body)
                .filter(|summary| !summary.is_empty());

            match body_summary {
                Some(summary) => format!("上游 HTTP {status}: {summary}"),
                None => format!("上游 HTTP {status}"),
            }
        }
        ProxyError::Timeout(message) => {
            format!("请求超时: {}", summarize_text_for_log(message, 180))
        }
        ProxyError::ForwardFailed(message) => {
            format!("请求转发失败: {}", summarize_text_for_log(message, 180))
        }
        ProxyError::TransformError(message) => {
            format!("响应转换失败: {}", summarize_text_for_log(message, 180))
        }
        ProxyError::ConfigError(message) => {
            format!("配置错误: {}", summarize_text_for_log(message, 180))
        }
        ProxyError::AuthError(message) => {
            format!("认证失败: {}", summarize_text_for_log(message, 180))
        }
        _ => summarize_text_for_log(&error.to_string(), 180),
    }
}

fn summarize_upstream_body(body: &str) -> String {
    if let Ok(json_body) = serde_json::from_str::<Value>(body) {
        if let Some(message) = extract_json_error_message(&json_body) {
            return summarize_text_for_log(&message, 180);
        }

        if let Ok(compact_json) = serde_json::to_string(&json_body) {
            return summarize_text_for_log(&compact_json, 180);
        }
    }

    summarize_text_for_log(body, 180)
}

fn extract_json_error_message(body: &Value) -> Option<String> {
    let candidates = [
        body.pointer("/error/message"),
        body.pointer("/message"),
        body.pointer("/detail"),
        body.pointer("/error"),
    ];

    candidates
        .into_iter()
        .flatten()
        .find_map(|value| value.as_str().map(ToString::to_string))
}

fn split_endpoint_and_query(endpoint: &str) -> (&str, Option<&str>) {
    endpoint
        .split_once('?')
        .map_or((endpoint, None), |(path, query)| (path, Some(query)))
}

fn strip_beta_query(query: Option<&str>) -> Option<String> {
    let filtered = query.map(|query| {
        query
            .split('&')
            .filter(|pair| !pair.is_empty() && !pair.starts_with("beta="))
            .collect::<Vec<_>>()
            .join("&")
    });

    match filtered.as_deref() {
        Some("") | None => None,
        Some(_) => filtered,
    }
}

fn is_claude_messages_path(path: &str) -> bool {
    matches!(path, "/v1/messages" | "/claude/v1/messages")
}

fn rewrite_codex_responses_endpoint_to_chat(endpoint: &str) -> (String, Option<String>) {
    let (_path, query) = split_endpoint_and_query(endpoint);
    let passthrough_query = query.map(ToString::to_string);
    let target_path = "/chat/completions";
    let rewritten = match passthrough_query.as_deref() {
        Some(query) if !query.is_empty() => format!("{target_path}?{query}"),
        _ => target_path.to_string(),
    };

    (rewritten, passthrough_query)
}

/// Claude Code client fingerprint (used for Codex→Anthropic emulation to pass a
/// gateway's "Claude Code only" check).
const CLAUDE_CODE_USER_AGENT: &str = "claude-cli/1.0.119 (external, cli)";
const CLAUDE_CODE_SYSTEM_IDENTITY: &str =
    "You are Claude Code, Anthropic's official CLI for Claude.";

/// Insert the Claude Code identity as the first line before the `system` field in
/// the Anthropic request body.
///
/// Anthropic subscription/OAuth plans require the first system block to be exactly
/// this identity line. After conversion `system` is a string (from Codex
/// instructions); normalize it into an array here: [identity line, original system...].
fn prepend_claude_code_system_prompt(body: &mut Value) {
    let identity = serde_json::json!({ "type": "text", "text": CLAUDE_CODE_SYSTEM_IDENTITY });
    let mut blocks: Vec<Value> = vec![identity];
    match body.get("system") {
        Some(Value::String(existing)) if !existing.is_empty() => {
            blocks.push(serde_json::json!({ "type": "text", "text": existing }));
        }
        Some(Value::Array(existing)) => {
            // Idempotent: skip re-injection if the first block is already the identity line.
            if existing
                .first()
                .and_then(|b| b.get("text"))
                .and_then(|t| t.as_str())
                == Some(CLAUDE_CODE_SYSTEM_IDENTITY)
            {
                return;
            }
            blocks.extend(existing.iter().cloned());
        }
        _ => {}
    }
    body["system"] = Value::Array(blocks);
}

/// Headers a native Claude Code client never sends but the Codex/OpenAI CLI (and its
/// stainless SDK layer) do. Dropped for every Codex→Anthropic request so the upstream sees a
/// clean Anthropic client fingerprint. Centralized here so the set stays in one place and future
/// additions can't miss a code path. `key_str` is already lowercased by the http crate.
/// Whether `base_url` already ends in `endpoint_suffix` (e.g. `/v1/messages` or
/// `/chat/completions`), ignoring surrounding whitespace, any `?query`/`#fragment`, and a
/// trailing slash. Used to avoid double-appending the endpoint when a user pastes a full
/// URL but leaves the "full URL" switch off (`.../v1/messages` → `.../v1/messages/v1/messages`,
/// a non-retryable 400). `endpoint_suffix` must be lowercase.
fn base_url_is_full_endpoint(base_url: &str, endpoint_suffix: &str) -> bool {
    let trimmed = base_url.trim();
    // Match against the path only: a `?query`/`#fragment` on a full endpoint URL must not
    // hide the suffix (`.../v1/messages?beta=true` still ends in the endpoint).
    let path = match trimmed.split_once(['?', '#']) {
        Some((head, _)) => head,
        None => trimmed,
    };
    path.trim_end_matches('/')
        .to_ascii_lowercase()
        .ends_with(endpoint_suffix)
}

fn is_codex_client_fingerprint_header(key_str: &str) -> bool {
    matches!(
        key_str,
        "originator"
            | "session_id"
            | "session-id"
            | "thread-id"
            | "conversation_id"
            | "chatgpt-account-id"
            | "x-client-request-id"
            | "openai-beta"
            | "openai-organization"
            | "openai-project"
            // Codex 内置 openai 的固定请求头（值是 Codex 版本号）。
            | "version"
    ) || key_str.starts_with("x-stainless-")
        || key_str.starts_with("x-codex-")
        // ChatGPT 登录态下 Codex 每个请求都带：`x-oai-attestation`（设备证明）、
        // `x-openai-fedramp`、`x-openai-account-routing-override`、`x-openai-memgen-request`、
        // `x-openai-internal-codex-responses-lite`、`x-responsesapi-include-timing-metrics`。
        || key_str.starts_with("x-oai-")
        || key_str.starts_with("x-openai-")
        || key_str.starts_with("x-responsesapi-")
}

/// Decide whether a Codex fingerprint header must be removed before the request
/// reaches an upstream.
///
/// Stack requests to a third party still must not carry ChatGPT session or
/// account identity. A native Responses third party, however, may use the
/// non-identity `x-codex-beta-features` marker to recognize the Codex protocol
/// and reject requests without it. Keep only that marker; all other Codex and
/// OpenAI identity headers remain stripped. Converted Chat/Anthropic requests
/// continue to receive a clean client fingerprint.
///
/// Any other Codex request to a third party (`codex_unmanaged_upstream`: Codex and
/// not a CC Switch-managed ChatGPT account) drops only the ChatGPT account identity
/// ([`is_chatgpt_identity_header`]): the client may be the built-in `openai`
/// provider carrying the user's ChatGPT login. Session and client headers stay,
/// so requests from a plain custom table are unchanged.
fn should_strip_codex_client_fingerprint_header(
    key_str: &str,
    endpoint: &str,
    codex_responses_to_anthropic: bool,
    codex_responses_to_chat: bool,
    codex_stack_request: bool,
    codex_official_auth_passthrough: bool,
    codex_unmanaged_upstream: bool,
) -> bool {
    if codex_responses_to_anthropic {
        return is_codex_client_fingerprint_header(key_str);
    }

    if codex_stack_request && !codex_official_auth_passthrough {
        let native_third_party_responses =
            !codex_responses_to_chat && super::providers::is_codex_responses_endpoint(endpoint);
        return is_codex_client_fingerprint_header(key_str)
            && !(native_third_party_responses && key_str == "x-codex-beta-features");
    }

    codex_unmanaged_upstream
        && !codex_official_auth_passthrough
        && is_chatgpt_identity_header(key_str)
}

/// 只有带着 ChatGPT 登录的内置 openai 才会发的请求头：账号、组织 / 项目、设备证明和
/// 内置 openai 的固定版本头。会话、客户端标识（`session_id`、`originator`、`x-codex-*`）
/// 任何 Codex 客户端都发，不在其中。
fn is_chatgpt_identity_header(key_str: &str) -> bool {
    matches!(
        key_str,
        "chatgpt-account-id" | "openai-organization" | "openai-project" | "version"
    ) || key_str.starts_with("x-oai-")
        || key_str.starts_with("x-openai-")
        || key_str.starts_with("x-responsesapi-")
}

fn codex_anthropic_error_envelope_message(body: &[u8]) -> Option<String> {
    let value: Value = serde_json::from_slice(body).ok()?;
    if value.get("type").and_then(Value::as_str) != Some("error") && value.get("error").is_none() {
        return None;
    }
    let error = value.get("error").unwrap_or(&value);
    let error_type = error.get("type").and_then(Value::as_str).unwrap_or("error");
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| error.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| error.to_string());
    Some(format!("{error_type}: {message}"))
}

fn responses_error_envelope_message(body: &[u8]) -> Option<String> {
    let value: Value = serde_json::from_slice(body).ok()?;
    let status = value.get("status").and_then(Value::as_str);
    let has_error = value.get("error").is_some_and(|error| !error.is_null());
    if !matches!(status, Some("failed" | "cancelled")) && !has_error {
        return None;
    }

    let error = value.get("error").unwrap_or(&value);
    let error_type = error
        .get("type")
        .and_then(Value::as_str)
        .or_else(|| error.get("code").and_then(Value::as_str))
        .unwrap_or_else(|| status.unwrap_or("error"));
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| error.as_str())
        .filter(|message| !message.trim().is_empty())
        .unwrap_or(match status {
            Some("cancelled") => "response generation was cancelled",
            _ => "response generation failed",
        });
    Some(format!("{error_type}: {message}"))
}

/// Prompt caching is part of the Codex→Anthropic protocol bridge rather than an
/// optional Bedrock optimizer. Codex requests do not contain Anthropic
/// `cache_control`, so keep bridge caching on by default while still honoring the
/// dedicated cache-injection switch. Injected breakpoints always use Anthropic's
/// standard 5-minute TTL.
fn codex_anthropic_cache_config(config: &OptimizerConfig) -> OptimizerConfig {
    OptimizerConfig {
        enabled: true,
        thinking_optimizer: false,
        cache_injection: config.cache_injection,
    }
}

/// [`Forwarder::hold_encrypted_agent_task_stream`] 看到一个完整 SSE 块之后怎么做。
enum StreamStart {
    /// 生命周期事件（或不是 JSON），接着读。
    Pending,
    /// 有输出或别的结局：交出流。
    Commit,
    /// 输出之前上游就自证验不了请求里的密文。
    Rejected(ProxyError),
}

fn encrypted_task_stream_start(block: &str, request: &Value) -> StreamStart {
    let mut named_event = None;
    let mut data_lines = Vec::new();
    for line in block.lines() {
        if let Some(event) = crate::proxy::sse::strip_sse_field(line, "event") {
            named_event = Some(event.trim().to_string());
        } else if let Some(data) = crate::proxy::sse::strip_sse_field(line, "data") {
            data_lines.push(data);
        }
    }
    let Ok(value) = serde_json::from_str::<Value>(&data_lines.join("\n")) else {
        return StreamStart::Pending;
    };
    let event = named_event
        .as_deref()
        .filter(|event| !event.is_empty())
        .or_else(|| value.get("type").and_then(Value::as_str))
        .unwrap_or("");
    match event {
        "response.created" | "response.in_progress" | "response.queued" => StreamStart::Pending,
        "response.failed" | "error" => {
            let response = value.get("response").unwrap_or(&value);
            let error = response.get("error").unwrap_or(response);
            match super::opaque_state_rectifier::in_stream_rejection(error, request) {
                Some(rejection) => StreamStart::Rejected(rejection),
                None => StreamStart::Commit,
            }
        }
        _ => StreamStart::Commit,
    }
}

/// A streaming request may receive a whole JSON document even when the gateway
/// omits `application/json`. `None` means either "not JSON" or "not complete yet";
/// a parsed document is safe to commit unless it is a semantic failure envelope.
fn inspect_responses_json_document(buffer: &str) -> Option<Result<(), ProxyError>> {
    let trimmed = buffer.trim();
    if !matches!(trimmed.as_bytes().first(), Some(b'{') | Some(b'[')) {
        return None;
    }
    let _: Value = serde_json::from_str(trimmed).ok()?;
    if let Some(message) = responses_error_envelope_message(trimmed.as_bytes()) {
        return Some(Err(ProxyError::TransformError(format!(
            "Responses upstream returned a 2xx failure: {message}"
        ))));
    }
    Some(Ok(()))
}

/// Inspect one complete Responses SSE block while the response is still inside
/// the retry loop. `None` means the event is lifecycle-only and priming should
/// continue; `Some(Ok(()))` means it is safe to commit/replay the stream.
fn inspect_responses_start_event(block: &str) -> Option<Result<(), ProxyError>> {
    let mut named_event = None;
    let mut data_lines = Vec::new();
    for line in block.lines() {
        if let Some(event) = crate::proxy::sse::strip_sse_field(line, "event") {
            named_event = Some(event.trim().to_string());
        } else if let Some(data) = crate::proxy::sse::strip_sse_field(line, "data") {
            data_lines.push(data);
        }
    }
    if data_lines.is_empty() {
        return None;
    }
    let value: Value = match serde_json::from_str(&data_lines.join("\n")) {
        Ok(value) => value,
        Err(_) => return None,
    };
    let event = named_event
        .as_deref()
        .filter(|event| !event.is_empty())
        .or_else(|| value.get("type").and_then(Value::as_str))
        .unwrap_or("");

    let response = value.get("response").unwrap_or(&value);
    if matches!(
        response.get("status").and_then(Value::as_str),
        Some("failed" | "cancelled")
    ) || response.get("error").is_some_and(|error| !error.is_null())
    {
        let error = response.get("error").unwrap_or(response);
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .or_else(|| error.as_str())
            .unwrap_or("Responses upstream failed before output");
        let error_type = error
            .get("type")
            .and_then(Value::as_str)
            .or_else(|| error.get("code").and_then(Value::as_str))
            .or_else(|| response.get("status").and_then(Value::as_str))
            .unwrap_or("upstream_error");
        return Some(Err(ProxyError::TransformError(format!(
            "Responses upstream {error_type}: {message}"
        ))));
    }

    match event {
        "response.failed" | "error" => {
            let error = response.get("error").unwrap_or(response);
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .or_else(|| error.as_str())
                .unwrap_or("Responses upstream emitted an error before output");
            let error_type = error
                .get("type")
                .and_then(Value::as_str)
                .or_else(|| error.get("code").and_then(Value::as_str))
                .unwrap_or("upstream_error");
            Some(Err(ProxyError::TransformError(format!(
                "Responses upstream {error_type}: {message}"
            ))))
        }
        "response.created" | "response.in_progress" | "response.queued" => None,
        "" => None,
        // Productive output, incomplete, and completed terminals are all safe to
        // expose. Mid-stream failures after this point are surfaced by the converter
        // but intentionally do not switch providers.
        _ => Some(Ok(())),
    }
}

/// Rewrite Codex's `/responses` (and variants) to Anthropic's `/v1/messages`, preserving the query.
fn rewrite_codex_responses_endpoint_to_anthropic(endpoint: &str) -> (String, Option<String>) {
    let (_path, query) = split_endpoint_and_query(endpoint);
    let passthrough_query = query.map(ToString::to_string);
    let target_path = "/v1/messages";
    let rewritten = match passthrough_query.as_deref() {
        Some(query) if !query.is_empty() => format!("{target_path}?{query}"),
        _ => target_path.to_string(),
    };

    (rewritten, passthrough_query)
}

fn is_managed_copilot_request(app_type: &AppType, provider: &Provider, base_url: &str) -> bool {
    provider
        .meta
        .as_ref()
        .and_then(|meta| meta.provider_type.as_deref())
        == Some("github_copilot")
        || (!matches!(app_type, AppType::Codex) && base_url.contains("githubcopilot.com"))
}

fn codex_copilot_lookup_error(model_id: &str, error: CopilotAuthError) -> ProxyError {
    let message =
        format!("Failed to resolve GitHub Copilot capabilities for model {model_id}: {error}");
    match error {
        CopilotAuthError::DeviceFlowNotStarted
        | CopilotAuthError::AuthorizationPending
        | CopilotAuthError::AccessDenied
        | CopilotAuthError::ExpiredToken
        | CopilotAuthError::GitHubTokenInvalid
        | CopilotAuthError::NoCopilotSubscription
        | CopilotAuthError::AccountNotFound(_) => ProxyError::AuthError(message),
        CopilotAuthError::NetworkError(_) | CopilotAuthError::CopilotTokenFetchFailed(_) => {
            ProxyError::ForwardFailed(message)
        }
        CopilotAuthError::ParseError(_)
        | CopilotAuthError::IoError(_)
        | CopilotAuthError::InvalidDomain(_) => ProxyError::ConfigError(message),
    }
}

fn apply_codex_copilot_model(
    body: &mut Value,
    resolved: Option<ResolvedCopilotModel>,
    api_format: CodexCopilotApiFormat,
) -> Result<CopilotTransport, ProxyError> {
    let requested = match api_format {
        CodexCopilotApiFormat::Auto => "a Responses or Chat Completions endpoint",
        CodexCopilotApiFormat::OpenaiResponses => {
            "the requested Responses endpoint (codexCopilotApiFormat=openai_responses)"
        }
        CodexCopilotApiFormat::OpenaiChat => {
            "the requested Chat Completions endpoint (codexCopilotApiFormat=openai_chat)"
        }
    };
    let Some(resolved) = resolved else {
        let model = body
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or("<missing>");
        return Err(ProxyError::ConfigError(format!(
            "GitHub Copilot model {model} is unavailable for {requested} in this provider's catalogue"
        )));
    };
    let Some(transport) = resolved.transport else {
        return Err(ProxyError::ConfigError(format!(
            "GitHub Copilot model {} does not advertise {requested}",
            resolved.id
        )));
    };
    body["model"] = Value::String(resolved.id);
    Ok(transport)
}

fn rewrite_codex_endpoint_for_copilot(
    inbound_endpoint: &str,
    supported_endpoint: &str,
) -> (String, Option<String>) {
    let (inbound_path, query) = split_endpoint_and_query(inbound_endpoint);
    let passthrough_query = query.map(ToString::to_string);
    let mut target_path = supported_endpoint.trim_end_matches('/').to_string();
    if inbound_path.ends_with("/responses/compact")
        && matches!(target_path.as_str(), "/responses" | "/v1/responses")
    {
        target_path.push_str("/compact");
    }
    let rewritten = match passthrough_query.as_deref() {
        Some(query) if !query.is_empty() => format!("{target_path}?{query}"),
        _ => target_path,
    };
    (rewritten, passthrough_query)
}

/// Providers whose errors can come from an Anthropic Messages endpoint and therefore
/// need the thinking signature / budget rectifiers. GitHub Copilot passes Anthropic
/// models through to its native `/v1/messages`, where history replayed from another
/// provider or from the Responses bridge fails signature validation; errors from its
/// Chat / Responses bridges never match the rectifier keywords.
fn uses_anthropic_rectifiers(provider_type: Option<ProviderType>) -> bool {
    matches!(
        provider_type,
        Some(ProviderType::Claude | ProviderType::ClaudeAuth | ProviderType::GitHubCopilot)
    )
}

/// Pick the Copilot upstream protocol for a Claude-client request.
///
/// Anthropic models that advertise `/v1/messages` are passed through natively:
/// the Chat Completions bridge drops `cache_control` breakpoints and Copilot's
/// chat endpoint stops prefix caching at the first mid-conversation `system`
/// message Claude Code inserts, so every turn re-bills the whole history.
fn copilot_api_format_for_model(
    vendor: Option<&str>,
    supported_endpoints: &[String],
) -> &'static str {
    if vendor.is_some_and(|vendor| vendor.eq_ignore_ascii_case("openai")) {
        "openai_responses"
    } else if vendor.is_some_and(|vendor| vendor.eq_ignore_ascii_case("anthropic"))
        && supported_endpoints
            .iter()
            .any(|endpoint| endpoint.trim_end_matches('/') == "/v1/messages")
    {
        "anthropic"
    } else {
        "openai_chat"
    }
}

fn build_copilot_unversioned_url(base_url: &str, endpoint: &str) -> String {
    format!(
        "{}/{}",
        base_url.trim_end_matches('/'),
        endpoint.trim_start_matches('/')
    )
}

fn rewrite_claude_transform_endpoint(
    endpoint: &str,
    api_format: &str,
    is_copilot: bool,
    body: &Value,
) -> (String, Option<String>) {
    let (path, query) = split_endpoint_and_query(endpoint);
    let passthrough_query = if is_claude_messages_path(path) {
        strip_beta_query(query)
    } else {
        query.map(ToString::to_string)
    };

    if !is_claude_messages_path(path) {
        return (endpoint.to_string(), passthrough_query);
    }

    if api_format == "gemini_native" {
        let model =
            super::providers::transform_gemini::extract_gemini_model(body).unwrap_or("unknown");
        // Accept both bare ids (`gemini-2.5-pro`) and the resource-name
        // form (`models/gemini-2.5-pro`) that Gemini SDKs emit. See
        // `normalize_gemini_model_id` for rationale.
        let model = super::gemini_url::normalize_gemini_model_id(model);
        let is_stream = body
            .get("stream")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        let target_path = if is_stream {
            format!("/v1beta/models/{model}:streamGenerateContent")
        } else {
            format!("/v1beta/models/{model}:generateContent")
        };

        let rewritten_query = merge_query_params(
            passthrough_query.as_deref(),
            if is_stream { Some("alt=sse") } else { None },
        );

        let rewritten = match rewritten_query.as_deref() {
            Some(query) if !query.is_empty() => format!("{target_path}?{query}"),
            _ => target_path,
        };

        return (rewritten, rewritten_query);
    }

    let target_path = if is_copilot && api_format == "openai_responses" {
        "/v1/responses"
    } else if is_copilot {
        "/chat/completions"
    } else if api_format == "openai_responses" {
        "/v1/responses"
    } else {
        "/v1/chat/completions"
    };

    let rewritten = match passthrough_query.as_deref() {
        Some(query) if !query.is_empty() => format!("{target_path}?{query}"),
        _ => target_path.to_string(),
    };

    (rewritten, passthrough_query)
}

fn merge_query_params(base_query: Option<&str>, extra_param: Option<&str>) -> Option<String> {
    let mut params: Vec<String> = base_query
        .into_iter()
        .flat_map(|query| query.split('&'))
        .filter(|pair| !pair.is_empty())
        .filter(|pair| !pair.starts_with("alt="))
        .map(ToString::to_string)
        .collect();

    if let Some(extra_param) = extra_param {
        params.push(extra_param.to_string());
    }

    if params.is_empty() {
        None
    } else {
        Some(params.join("&"))
    }
}

fn append_query_to_full_url(base_url: &str, query: Option<&str>) -> String {
    match query {
        Some(query) if !query.is_empty() => {
            if base_url.contains('?') {
                format!("{base_url}&{query}")
            } else {
                format!("{base_url}?{query}")
            }
        }
        _ => base_url.to_string(),
    }
}

#[derive(Clone, Copy)]
enum CodexStandaloneEndpoint {
    AlphaSearch,
    ImagesGenerations,
    ImagesEdits,
}

impl CodexStandaloneEndpoint {
    fn from_effective_endpoint(endpoint: &str) -> Option<Self> {
        match split_endpoint_and_query(endpoint).0 {
            "/alpha/search" => Some(Self::AlphaSearch),
            "/images/generations" => Some(Self::ImagesGenerations),
            "/images/edits" => Some(Self::ImagesEdits),
            _ => None,
        }
    }

    fn path(self) -> &'static str {
        match self {
            Self::AlphaSearch => "/alpha/search",
            Self::ImagesGenerations => "/images/generations",
            Self::ImagesEdits => "/images/edits",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::AlphaSearch => "Codex Alpha Search",
            Self::ImagesGenerations => "Codex Images generations",
            Self::ImagesEdits => "Codex Images edits",
        }
    }

    fn full_url_hint(self) -> &'static str {
        match self {
            Self::AlphaSearch => "/responses",
            Self::ImagesGenerations | Self::ImagesEdits => {
                "/responses, /chat/completions, /images/generations, or /images/edits"
            }
        }
    }

    /// Full-URL suffixes that unambiguously locate this endpoint's sibling.
    ///
    /// Order matters: a longer suffix must precede any suffix it ends with
    /// (`/responses/compact` before `/responses`), otherwise the shorter one
    /// wins and the rewrite keeps a stray `/compact` segment.
    fn source_suffixes(self) -> &'static [&'static str] {
        match self {
            Self::AlphaSearch => &["/responses/compact", "/responses"],
            // Both Images routes live next to each other, so a full URL pasted
            // for either one is a valid source for the other.
            Self::ImagesGenerations | Self::ImagesEdits => &[
                "/images/generations",
                "/images/edits",
                "/chat/completions",
                "/responses/compact",
                "/responses",
            ],
        }
    }

    fn source_suffix(self, parsed_path: &str) -> Option<&'static str> {
        // Match the case-insensitive pasted-endpoint check. Only normalize for
        // matching; the rewrite keeps the original URL prefix and query intact.
        let parsed_path = parsed_path.to_ascii_lowercase();
        self.source_suffixes()
            .iter()
            .copied()
            .find(|suffix| parsed_path.ends_with(suffix))
    }

    /// Whether a base URL (full-URL switch off) already ends in one of this
    /// endpoint's source suffixes, i.e. was pasted as a complete endpoint URL.
    fn base_url_is_source_endpoint(self, base_url: &str) -> bool {
        self.source_suffixes()
            .iter()
            .any(|suffix| base_url_is_full_endpoint(base_url, suffix))
    }
}

/// Derive Codex standalone sibling endpoints from a provider configured with a
/// complete Codex-compatible API URL.
///
/// Full-URL mode normally means "use this exact URL". That is correct for the
/// request type it was configured for, but standalone Codex protocols cannot be
/// posted to chat/responses endpoints. Only rewrite URL shapes whose sibling
/// endpoint is unambiguous; opaque full URLs fail closed instead of leaking the
/// payload to an unrelated route.
fn rewrite_codex_standalone_full_url(
    base_url: &str,
    request_query: Option<&str>,
    endpoint: CodexStandaloneEndpoint,
) -> Result<String, ProxyError> {
    let trimmed = base_url.trim();
    let parsed = url::Url::parse(trimmed).map_err(|_| {
        ProxyError::ConfigError(format!("{} requires a valid full URL", endpoint.label()))
    })?;

    // Fragments are never sent in HTTP requests. Drop one before splitting the
    // query so an accidental fragment cannot move the incoming query behind `#`.
    let without_fragment = trimmed
        .split_once('#')
        .map_or(trimmed, |(head, _fragment)| head);
    let (url_without_query, base_query) = without_fragment
        .split_once('?')
        .map_or((without_fragment, None), |(head, query)| {
            (head, Some(query))
        });
    let url_without_query = url_without_query.trim_end_matches('/');

    let parsed_path = parsed.path().trim_end_matches('/').to_string();
    let suffix = endpoint.source_suffix(&parsed_path).ok_or_else(|| {
        ProxyError::ConfigError(format!(
            "{} cannot derive {} from an opaque full URL; use a base URL or a full URL ending in {}",
            endpoint.label(),
            endpoint.path(),
            endpoint.full_url_hint()
        ))
    })?;

    let prefix_len = url_without_query
        .len()
        .checked_sub(suffix.len())
        .ok_or_else(|| ProxyError::ConfigError("Invalid Codex full URL".to_string()))?;
    let mut rewritten = format!("{}{}", &url_without_query[..prefix_len], endpoint.path());

    let request_query = request_query.filter(|query| !query.is_empty());
    let base_query = base_query.filter(|query| !query.is_empty());
    match (base_query, request_query) {
        (Some(base), Some(request)) => rewritten.push_str(&format!("?{base}&{request}")),
        (Some(base), None) => rewritten.push_str(&format!("?{base}")),
        (None, Some(request)) => rewritten.push_str(&format!("?{request}")),
        (None, None) => {}
    }

    Ok(rewritten)
}

fn build_codex_oauth_session_headers(
    session_id: &str,
) -> Vec<(http::HeaderName, http::HeaderValue)> {
    let session_id = session_id.trim();
    if session_id.is_empty() {
        return Vec::new();
    }

    let mut headers = Vec::new();
    if let Ok(value) = http::HeaderValue::from_str(session_id) {
        headers.push((http::HeaderName::from_static("session_id"), value.clone()));
        headers.push((http::HeaderName::from_static("x-client-request-id"), value));
    }

    let window_id = format!("{session_id}:0");
    if let Ok(value) = http::HeaderValue::from_str(&window_id) {
        headers.push((http::HeaderName::from_static("x-codex-window-id"), value));
    }

    headers
}

fn reject_proxy_placeholder_for_managed_account_upstream(
    url: &str,
    headers: &http::HeaderMap,
) -> Result<(), ProxyError> {
    if !is_managed_account_upstream_url(url) || !headers_contain_proxy_placeholder(headers) {
        return Ok(());
    }

    Err(ProxyError::AuthError(
        "Managed account proxy auth was not resolved; PROXY_MANAGED must not be sent upstream"
            .to_string(),
    ))
}

fn is_managed_account_upstream_url(url: &str) -> bool {
    let Ok(uri) = url.parse::<http::Uri>() else {
        return false;
    };

    let Some(host) = uri.host().map(str::to_ascii_lowercase) else {
        return false;
    };

    host == "githubcopilot.com"
        || host.ends_with(".githubcopilot.com")
        || (host == "chatgpt.com" && uri.path().starts_with("/backend-api/codex"))
        || (host == "api.x.ai" && uri.path().starts_with("/v1/"))
}

fn headers_contain_proxy_placeholder(headers: &http::HeaderMap) -> bool {
    headers.values().any(|value| {
        value
            .to_str()
            .map(|value| value.contains(PROXY_AUTH_PLACEHOLDER))
            .unwrap_or(false)
    })
}

fn should_preserve_exact_header_case(
    adapter_name: &str,
    provider: &Provider,
    resolved_claude_api_format: Option<&str>,
    is_copilot: bool,
) -> bool {
    if matches!(adapter_name, "Codex" | "Gemini") {
        return false;
    }

    if is_copilot || provider.is_codex_oauth() || provider.is_xai_oauth() {
        return false;
    }

    matches!(resolved_claude_api_format, None | Some("anthropic"))
}

fn is_streaming_request(endpoint: &str, body: &Value, headers: &axum::http::HeaderMap) -> bool {
    if body
        .get("stream")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
    {
        return true;
    }

    if endpoint.contains("streamGenerateContent") || endpoint.contains("alt=sse") {
        return true;
    }

    headers
        .get(axum::http::header::ACCEPT)
        .and_then(|value| value.to_str().ok())
        .map(|accept| accept.contains("text/event-stream"))
        .unwrap_or(false)
}

#[cfg(test)]
fn should_force_identity_encoding(
    endpoint: &str,
    body: &Value,
    headers: &axum::http::HeaderMap,
) -> bool {
    is_streaming_request(endpoint, body, headers)
}

fn map_reqwest_send_error(error: reqwest::Error) -> ProxyError {
    let is_timeout = error.is_timeout();
    let is_connect = error.is_connect();
    let detail = super::error::describe_reqwest_error(error);
    if is_timeout {
        ProxyError::Timeout(format!("上游请求超时: {detail}"))
    } else if is_connect {
        ProxyError::ForwardFailed(format!("上游连接失败: {detail}"))
    } else {
        ProxyError::ForwardFailed(format!("上游请求发送失败: {detail}"))
    }
}

fn summarize_text_for_log(text: &str, max_chars: usize) -> String {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = normalized.trim();

    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }

    let truncated: String = trimmed.chars().take(max_chars).collect();
    let truncated = truncated.trim_end();
    format!("{truncated}...")
}

fn apply_local_proxy_body_overrides(
    body: &mut Value,
    overrides: &LocalProxyRequestOverrides,
) -> bool {
    let Some(override_body) = overrides.body.as_ref() else {
        return false;
    };

    if !override_body.is_object() {
        log::warn!("[LocalProxyOverrides] Ignoring body override because it is not an object");
        return false;
    }

    merge_json_override(body, override_body)
}

fn merge_json_override(target: &mut Value, patch: &Value) -> bool {
    merge_json_override_inner(target, patch, true)
}

fn merge_json_override_inner(target: &mut Value, patch: &Value, is_top_level: bool) -> bool {
    match (target, patch) {
        (Value::Object(target_map), Value::Object(patch_map)) => {
            let mut changed = false;
            for (key, patch_value) in patch_map {
                if is_top_level && key == "stream" {
                    log::warn!(
                        "[LocalProxyOverrides] Ignoring body override for protected field: stream"
                    );
                    continue;
                }
                match target_map.get_mut(key) {
                    Some(target_value) => {
                        changed |= merge_json_override_inner(target_value, patch_value, false);
                    }
                    None => {
                        target_map.insert(key.clone(), patch_value.clone());
                        changed = true;
                    }
                }
            }
            changed
        }
        (target_value, patch_value) => {
            if target_value == patch_value {
                false
            } else {
                *target_value = patch_value.clone();
                true
            }
        }
    }
}

/// Inspect one complete SSE block for a semantic error event.
///
/// `None` means the block carries no error and priming can commit the stream.
/// `Some(Err(_))` wraps the upstream's error body as `UpstreamError { status: 200 }`
/// — the in-stream form of a rejection (Anthropic gateways answer a converted
/// parameter they don't support with `event: error` inside a 200) — so the
/// unsupported-parameter rectifier, which accepts that form, can strip and retry;
/// errors that name no parameter surface intact instead of aborting mid-stream.
fn inspect_sse_error_event(block: &str) -> Option<Result<(), ProxyError>> {
    let (event, data) = crate::proxy::sse::scan_sse_block(block);
    let data = data?;
    let is_error = event.is_some_and(|name| name == "error")
        || serde_json::from_str::<Value>(&data)
            .ok()
            .is_some_and(|value| {
                value.get("type").and_then(Value::as_str) == Some("error")
                    || value.get("error").is_some_and(|error| !error.is_null())
            });
    if !is_error {
        return None;
    }
    Some(Err(ProxyError::UpstreamError {
        status: 200,
        body: Some(data.into_owned()),
    }))
}

fn apply_local_proxy_header_overrides(
    headers: &mut http::HeaderMap,
    overrides: Option<&LocalProxyRequestOverrides>,
    is_copilot: bool,
) {
    if is_copilot {
        return;
    }

    let Some(header_overrides) = overrides.map(|overrides| &overrides.headers) else {
        return;
    };

    for (raw_name, raw_value) in header_overrides {
        let header_name = raw_name.trim().to_ascii_lowercase();
        if header_name.is_empty() {
            log::warn!("[LocalProxyOverrides] Ignoring header override with empty name");
            continue;
        }

        let Ok(name) = http::HeaderName::from_bytes(header_name.as_bytes()) else {
            log::warn!("[LocalProxyOverrides] Ignoring invalid header override name: {raw_name}");
            continue;
        };

        if is_protected_local_proxy_override_header(&name) {
            log::debug!(
                "[LocalProxyOverrides] Ignoring protected header override: {}",
                name.as_str()
            );
            continue;
        }

        let Ok(value) = http::HeaderValue::from_str(raw_value) else {
            log::warn!(
                "[LocalProxyOverrides] Ignoring invalid header override value for {}",
                name.as_str()
            );
            continue;
        };

        headers.insert(name, value);
    }
}

fn is_protected_local_proxy_override_header(name: &http::HeaderName) -> bool {
    matches!(
        name.as_str(),
        "host"
            | "content-length"
            | "transfer-encoding"
            | "connection"
            | "proxy-authorization"
            | "proxy-authenticate"
            | "te"
            | "trailer"
            | "upgrade"
            | "accept-encoding"
            | "content-type"
            | "authorization"
            | "x-api-key"
            | "x-goog-api-key"
            | "chatgpt-account-id"
            | "session_id"
            | "x-client-request-id"
            | "x-codex-window-id"
            | "x-forwarded-host"
            | "x-forwarded-port"
            | "x-forwarded-proto"
            | "forwarded"
            | "cf-connecting-ip"
            | "cf-ipcountry"
            | "cf-ray"
            | "cf-visitor"
            | "true-client-ip"
            | "fastly-client-ip"
            | "x-azure-clientip"
            | "x-azure-fdid"
            | "x-azure-ref"
            | "akamai-origin-hop"
            | "x-akamai-config-log-detail"
            | "x-request-id"
            | "x-correlation-id"
            | "x-trace-id"
            | "x-amzn-trace-id"
            | "x-b3-traceid"
            | "x-b3-spanid"
            | "x-b3-parentspanid"
            | "x-b3-sampled"
            | "traceparent"
            | "tracestate"
    )
}

fn log_prompt_cache_trace(
    app_type: &AppType,
    provider: &Provider,
    endpoint: &str,
    api_format: Option<&str>,
    body: &Value,
    session_client_provided: bool,
) {
    if !log::log_enabled!(log::Level::Debug) {
        return;
    }

    let prompt_cache_key = body
        .get("prompt_cache_key")
        .and_then(|value| value.as_str())
        .map(|key| format!("present(len={})", key.len()))
        .unwrap_or_else(|| "absent".to_string());
    let store = body
        .get("store")
        .map(value_for_log)
        .unwrap_or_else(|| "absent".to_string());
    let stream = body
        .get("stream")
        .map(value_for_log)
        .unwrap_or_else(|| "absent".to_string());
    let cache_controls = cache_control_summary(body);

    log::debug!(
        "[CacheTrace] app={}, provider={}, endpoint={}, api_format={}, session_client_provided={}, prompt_cache_key={}, store={}, stream={}, instructions_hash={}, system_hash={}, tools_hash={}, input_hash={}, messages_hash={}, include_hash={}, cache_controls={}, body_hash={}",
        app_type.as_str(),
        provider.id,
        // Gemini 的 endpoint 带 ?key=<API_KEY>；脱敏剥掉 query 再落盘。
        crate::redact_url_for_log(endpoint),
        api_format.unwrap_or("native"),
        session_client_provided,
        prompt_cache_key,
        store,
        stream,
        short_value_hash(body.get("instructions")),
        short_value_hash(body.get("system")),
        short_value_hash(body.get("tools")),
        short_value_hash(body.get("input")),
        short_value_hash(body.get("messages")),
        short_value_hash(body.get("include")),
        cache_controls,
        short_value_hash(Some(body)),
    );
}

fn cache_control_summary(value: &Value) -> String {
    fn walk(value: &Value, count: &mut usize, ttls: &mut std::collections::BTreeSet<String>) {
        match value {
            Value::Object(object) => {
                if let Some(cache_control) = object.get("cache_control") {
                    *count += 1;
                    let ttl = cache_control
                        .get("ttl")
                        .and_then(Value::as_str)
                        .unwrap_or("default");
                    ttls.insert(ttl.to_string());
                }
                for child in object.values() {
                    walk(child, count, ttls);
                }
            }
            Value::Array(items) => {
                for child in items {
                    walk(child, count, ttls);
                }
            }
            _ => {}
        }
    }

    let mut count = 0;
    let mut ttls = std::collections::BTreeSet::new();
    walk(value, &mut count, &mut ttls);
    format!(
        "count={count},ttls={}",
        if ttls.is_empty() {
            "none".to_string()
        } else {
            ttls.into_iter().collect::<Vec<_>>().join("|")
        }
    )
}

fn value_for_log(value: &Value) -> String {
    match value {
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => value.clone(),
        Value::Null => "null".to_string(),
        Value::Array(values) => format!("array(len={})", values.len()),
        Value::Object(values) => format!("object(len={})", values.len()),
    }
}

#[cfg(test)]
#[path = "forwarder_tests.rs"]
mod tests;
