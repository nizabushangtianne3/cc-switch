/**
 * Hermes Agent provider presets configuration
 * Hermes uses custom_providers array in config.yaml
 */
import { PRESET_BRAND_IDENTITIES } from "./presetBrandIdentities";
import type { ProviderCategory } from "../types";
import type { PresetTheme, TemplateValueConfig } from "./claudeProviderPresets";
import type { PresetFamilyFields } from "./presetFamilies";

/**
 * A model entry under a Hermes custom_provider.
 *
 * Serialized to YAML as a dict keyed by `id`:
 *
 * ```yaml
 * models:
 *   anthropic/claude-opus-5:
 *     context_length: 200000
 * ```
 *
 * Hermes' `_VALID_CUSTOM_PROVIDER_FIELDS` (hermes_cli/config.py) does not include
 * `max_tokens` at the per-model level — writing it produces an "unknown field"
 * warning on Hermes startup. Max tokens is a per-request parameter, not a
 * provider-level config.
 */
export interface HermesModel {
  /** Model ID — becomes the YAML key and the value written to top-level model.default. */
  id: string;
  /** Optional display label (UI only, not serialized to YAML). */
  name?: string;
  /** Override the auto-detected context window. */
  context_length?: number;
  /**
   * Declares that the model accepts image input. Hermes reads it per model
   * (`agent/image_routing.py`, `vision` is an alias); when absent it falls
   * back to its own catalog and treats unknown models as text-only.
   */
  supports_vision?: boolean;
}

/**
 * Top-level `model:` defaults suggested by a preset.
 *
 * Written to the YAML `model:` section when the user switches to this provider.
 * Per-model `context_length` lives on the individual `HermesModel` entries and
 * flows through `custom_providers[].models`, not this object.
 */
export interface HermesSuggestedDefaults {
  model: {
    /** Model ID for `model.default`. Typically equals `models[0].id`. */
    default: string;
    /** Value for `model.provider`. Omit to use the custom_provider name. */
    provider?: string;
  };
}

/** Hermes custom_provider protocol mode. Always written explicitly. */
export type HermesApiMode =
  | "chat_completions"
  | "anthropic_messages"
  | "codex_responses"
  | "bedrock_converse";

/** Default mode used when a provider has no stored value yet. */
export const HERMES_DEFAULT_API_MODE: HermesApiMode = "chat_completions";

/** Dropdown options for the API Mode selector. `labelKey` is looked up in i18n. */
export const hermesApiModes: Array<{
  value: HermesApiMode;
  labelKey: string;
}> = [
  { value: "chat_completions", labelKey: "hermes.form.apiModeChatCompletions" },
  {
    value: "anthropic_messages",
    labelKey: "hermes.form.apiModeAnthropicMessages",
  },
  { value: "codex_responses", labelKey: "hermes.form.apiModeCodexResponses" },
  {
    value: "bedrock_converse",
    labelKey: "hermes.form.apiModeBedrockConverse",
  },
];

export interface HermesProviderPreset extends PresetFamilyFields {
  name: string;
  nameKey?: string;
  websiteUrl: string;
  apiKeyUrl?: string;
  settingsConfig: HermesProviderSettingsConfig;
  isOfficial?: boolean;
  isPartner?: boolean;
  primePartner?: boolean; // 旧版的置顶合作伙伴标记；v7 起界面不再读取，新预设不写
  partnerPromotionKey?: string;
  category?: ProviderCategory;
  templateValues?: Record<string, TemplateValueConfig>;
  theme?: PresetTheme;
  icon?: string;
  iconColor?: string;
  isCustomTemplate?: boolean;
  /** Optional top-level `model:` defaults written on switch. */
  suggestedDefaults?: HermesSuggestedDefaults;
  /**
   * Provider key prefilled when the preset is picked. Set it when the obvious
   * name is a Hermes built-in provider id (e.g. `minimax`): Hermes routes
   * `model.provider` to the built-in, ignoring a same-named custom_provider.
   */
  providerKey?: string;
}

export interface HermesProviderSettingsConfig {
  name: string;
  base_url?: string;
  api_key?: string;
  api_mode?: HermesApiMode;
  /** UI-side ordered list; serialized to YAML as a dict keyed by id. */
  models?: HermesModel[];
  /** Delay in seconds between consecutive requests to this provider. */
  rate_limit_delay?: number;
  [key: string]: unknown;
}

export const hermesProviderPresets: HermesProviderPreset[] = [
  // ===== 赞助商预设：文件顺序与 README 赞助商表对齐（仅维护约定；应用内一律按显示名排序，不置顶）=====
  {
    name: "Kimi",
    family: "kimi",
    planKey: "payg",
    regionKey: "cn",
    primePartner: true,
    websiteUrl:
      "https://platform.kimi.com?track_id=track-7cf2b91dcde043eda6ef9a95951a042c&aff=cc-switch",
    settingsConfig: {
      name: "kimi",
      base_url: "https://api.moonshot.cn/v1",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        {
          id: "kimi-k3.1-pro",
          name: "Kimi K3.1 Pro",
          context_length: 1048576,
        },
        { id: "kimi-k2.7-code", name: "Kimi K2.7 Code" },
        { id: "kimi-k3", name: "Kimi K3", context_length: 1048576 },
        {
          id: "kimi-k2.7-code-highspeed",
          name: "Kimi K2.7 Code HighSpeed",
          context_length: 262144,
        },
        { id: "kimi-k2.6", name: "Kimi K2.6", context_length: 262144 },
      ],
    },
    category: "cn_official",
    partnerPromotionKey: "kimi",
    icon: "kimi",
    iconColor: "#6366F1",
    suggestedDefaults: {
      model: { default: "kimi-k2.7-code", provider: "kimi" },
    },
  },
  // API 开放平台海外/Global 变体：platform.kimi.ai + api.moonshot.ai 端点
  {
    name: "Kimi Global",
    family: "kimi",
    planKey: "payg",
    regionKey: "intl",
    websiteUrl:
      "https://platform.kimi.ai?track_id=track-674ed6e2af924a5682a87421f7cf753a&aff=cc-switch",
    settingsConfig: {
      name: "kimi",
      base_url: "https://api.moonshot.ai/v1",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        {
          id: "kimi-k3.1-pro",
          name: "Kimi K3.1 Pro",
          context_length: 1048576,
        },
        { id: "kimi-k2.7-code", name: "Kimi K2.7 Code" },
        { id: "kimi-k3", name: "Kimi K3", context_length: 1048576 },
        {
          id: "kimi-k2.7-code-highspeed",
          name: "Kimi K2.7 Code HighSpeed",
          context_length: 262144,
        },
        { id: "kimi-k2.6", name: "Kimi K2.6", context_length: 262144 },
      ],
    },
    category: "cn_official",
    partnerPromotionKey: "kimi",
    icon: "kimi",
    iconColor: "#6366F1",
    suggestedDefaults: {
      model: { default: "kimi-k2.7-code", provider: "kimi" },
    },
  },
  {
    name: "Kimi For Coding",
    family: "kimi",
    planKey: "coding",
    regionKey: "cn",
    primePartner: true,
    websiteUrl: "https://www.kimi.com/code/?aff=cc-switch",
    settingsConfig: {
      name: "kimi_coding",
      base_url: "https://api.kimi.com/coding/",
      api_key: "",
      api_mode: "anthropic_messages",
      models: [{ id: "kimi-for-coding", name: "Kimi For Coding" }],
    },
    category: "cn_official",
    icon: "kimi",
    iconColor: "#6366F1",
    suggestedDefaults: {
      model: { default: "kimi-for-coding", provider: "kimi_coding" },
    },
  },
  // 海外/Global 变体：kimi.ai/code + api.kimi.ai 端点，其余与国内版一致
  {
    name: "Kimi For Coding Global",
    family: "kimi",
    planKey: "coding",
    regionKey: "intl",
    websiteUrl: "https://www.kimi.ai/code?aff=cc-switch",
    settingsConfig: {
      name: "kimi_coding",
      base_url: "https://api.kimi.ai/coding/",
      api_key: "",
      api_mode: "anthropic_messages",
      models: [{ id: "kimi-for-coding", name: "Kimi For Coding" }],
    },
    category: "cn_official",
    icon: "kimi",
    iconColor: "#6366F1",
    suggestedDefaults: {
      model: { default: "kimi-for-coding", provider: "kimi_coding" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES.packycode,
    settingsConfig: {
      name: "packycode",
      base_url: "https://www.packyapi.ai",
      api_key: "",
      api_mode: "anthropic_messages",
      models: [
        {
          id: "claude-opus-5-5",
          name: "Claude Opus 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-sonnet-5-5",
          name: "Claude Sonnet 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-haiku-5-5",
          name: "Claude Haiku 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-fable-5-1",
          name: "Claude Fable 5.1",
          context_length: 1000000,
        },
      ],
    },
    suggestedDefaults: {
      model: { default: "claude-opus-5-5", provider: "packycode" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES.apinebula,
    settingsConfig: {
      name: "apinebula",
      base_url: "https://apinebula.ai/v1",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        { id: "gpt-6.1-sol", name: "GPT-6.1 Sol" },
        { id: "gpt-6-astra", name: "GPT-6 Astra", context_length: 272000 },
      ],
    },
    suggestedDefaults: {
      model: { default: "gpt-6.1-sol", provider: "apinebula" },
    },
  },
  {
    name: "AICodeMirror",
    websiteUrl: "https://www.aicodemirror.ai",
    apiKeyUrl: "https://www.aicodemirror.ai/register?invitecode=9915W3",
    settingsConfig: {
      name: "aicodemirror",
      base_url: "https://api.aicodemirror.ai/api/claudecode",
      api_key: "",
      api_mode: "anthropic_messages",
      models: [
        {
          id: "claude-opus-5-5",
          name: "Claude Opus 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-sonnet-5-5",
          name: "Claude Sonnet 5.5",
          context_length: 1000000,
        },
        { id: "claude-haiku-4-5-20251001", name: "Claude Haiku 4.5" },
      ],
    },
    category: "third_party",
    isPartner: true,
    partnerPromotionKey: "aicodemirror",
    icon: "aicodemirror",
    iconColor: "#000000",
    suggestedDefaults: {
      model: { default: "claude-opus-5-5", provider: "aicodemirror" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES.haoai,
    settingsConfig: {
      name: "hao_ai",
      base_url: "https://api.hao.ai/anthropic",
      api_key: "",
      api_mode: "anthropic_messages",
      models: [
        {
          id: "anthropic/claude-opus-5-5",
          name: "Claude Opus 5.5",
          context_length: 1000000,
        },
        {
          id: "anthropic/claude-sonnet-5-5",
          name: "Claude Sonnet 5.5",
          context_length: 1000000,
        },
      ],
    },
    suggestedDefaults: {
      model: { default: "anthropic/claude-opus-5-5", provider: "hao_ai" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES.fennoai,
    settingsConfig: {
      name: "fenno",
      base_url: "https://api.fenno.ai/v1",
      api_key: "",
      api_mode: "chat_completions",
      models: [{ id: "gpt-6.1-sol", name: "GPT-6.1 Sol" }],
    },
    suggestedDefaults: {
      model: { default: "gpt-6.1-sol", provider: "fenno" },
    },
  },
  {
    name: "RunAPI",
    websiteUrl: "https://runapi.host",
    apiKeyUrl: "https://runapi.host/register?aff=iOKB",
    settingsConfig: {
      name: "runapi",
      base_url: "https://runapi.host",
      api_key: "",
      api_mode: "anthropic_messages",
      models: [
        {
          id: "claude-opus-5-5",
          name: "Claude Opus 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-sonnet-5-5",
          name: "Claude Sonnet 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-haiku-5-5",
          name: "Claude Haiku 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-fable-5-1",
          name: "Claude Fable 5.1",
          context_length: 1000000,
        },
      ],
    },
    category: "aggregator",
    isPartner: true,
    partnerPromotionKey: "runapi",
    icon: "runapi",
    templateValues: {
      apiKey: {
        label: "API Key",
        placeholder: "",
        editorValue: "",
      },
    },
    suggestedDefaults: {
      model: { default: "claude-sonnet-5-5", provider: "runapi" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES.openmodel,
    settingsConfig: {
      name: "openmodel",
      base_url: "https://api.openmodel.ai",
      api_key: "",
      api_mode: "anthropic_messages",
      models: [
        {
          id: "claude-opus-5-5",
          name: "Claude Opus 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-sonnet-5-5",
          name: "Claude Sonnet 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-haiku-5-5",
          name: "Claude Haiku 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-fable-5-1",
          name: "Claude Fable 5.1",
          context_length: 1000000,
        },
      ],
    },
    suggestedDefaults: {
      model: { default: "claude-opus-5-5", provider: "openmodel" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES["grooroute-pro"],
    settingsConfig: {
      name: "grooroute_pro",
      base_url: "https://grooroute.pro",
      api_key: "",
      api_mode: "anthropic_messages",
      models: [
        {
          id: "claude-opus-5-5",
          name: "Claude Opus 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-sonnet-5-5",
          name: "Claude Sonnet 5.5",
          context_length: 1000000,
        },
      ],
    },
    suggestedDefaults: {
      model: { default: "claude-opus-5-5", provider: "grooroute_pro" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES.ditai,
    settingsConfig: {
      name: "dit_ai",
      base_url: "https://api.dit.ai",
      api_key: "",
      api_mode: "anthropic_messages",
      models: [
        {
          id: "claude-opus-5-5",
          name: "Claude Opus 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-sonnet-5-5",
          name: "Claude Sonnet 5.5",
          context_length: 1000000,
        },
      ],
    },
    suggestedDefaults: {
      model: { default: "claude-opus-5-5", provider: "dit_ai" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES.buytoken,
    settingsConfig: {
      name: "buytoken",
      base_url: "https://api.buytoken.work",
      api_key: "",
      api_mode: "anthropic_messages",
      models: [
        {
          id: "claude-opus-5-5",
          name: "Claude Opus 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-sonnet-5-5",
          name: "Claude Sonnet 5.5",
          context_length: 1000000,
        },
      ],
    },
    suggestedDefaults: {
      model: { default: "claude-opus-5-5", provider: "buytoken" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES["v-api"],
    settingsConfig: {
      name: "vapi",
      base_url: "https://api.gpt.ge",
      api_key: "",
      api_mode: "anthropic_messages",
      models: [
        {
          id: "claude-opus-5-5",
          name: "Claude Opus 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-sonnet-5-5",
          name: "Claude Sonnet 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-fable-5-1",
          name: "Claude Fable 5.1",
          context_length: 1000000,
        },
      ],
    },
    suggestedDefaults: {
      model: { default: "claude-opus-5-5", provider: "vapi" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES.tokenfactory,
    settingsConfig: {
      name: "tokenfactory",
      // 官方 Hermes 文档（docs.tokenfactory.cn/tools/hermes，2026-10-15 核对）
      base_url: "https://api.tokenfactory.cn/v1",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        { id: "glm-5.3", name: "GLM-5.3", context_length: 1048576 },
        { id: "kimi-k3", name: "Kimi K3", context_length: 1048576 },
        {
          id: "deepseek-v4-pro-0813",
          name: "DeepSeek V4 Pro",
          context_length: 1048576,
        },
        {
          id: "deepseek-v4.1-flash",
          name: "DeepSeek V4.1 Flash",
          context_length: 1048576,
        },
        {
          id: "deepseek-v4.1-flash-1017",
          name: "DeepSeek V4.1 Flash 1017",
          context_length: 1000000,
        },
        { id: "glm-5.3-flash", name: "GLM-5.3-Flash", context_length: 1048576 },
        { id: "qwen3.8-flash", name: "Qwen3.8 Flash", context_length: 1000000 },
      ],
    },
    suggestedDefaults: {
      model: { default: "glm-5.3", provider: "tokenfactory" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES["fluxa-token-plan"],
    // FluxA AgentMarket 以合作价转售的百度智能云 TokenPlan：产品页写明
    // "purchase it through AgentMarket, then use Baidu AI Cloud's endpoint and
    // API key directly"，端点取其所链的百度国际站 Token Plan Enterprise 文档
    // （2026-09-16 版）team 专属基址 —— 与国内个人版 qianfan.baidubce.com/
    // .../personal 是两套部署，勿合并。OpenAI 兼容基址走
    // chat_completions。阵容与窗口按 FluxA 产品页模型表
    // （glm-5.2 500k ≠ 国内版千帆平台 1M，国际 team 部署口径，勿按国内预设
    // "修正"）；标注 Coming soon 的 deepseek-v4-pro-0813 / glm-5.3 不收。
    // Kimi K2.6 是定稿赞助文案点名的模型，FluxA 产品页模型表与百度国际站
    // team 文档都没列它：id / 窗口取 FluxA baidu-ai-cloud 模型目录（categories
    // 只有 text）与国内 Token Plan 预设（262144）双重印证，非臆造
    settingsConfig: {
      name: "fluxa_tokenplan",
      base_url: "https://api.baiduqianfan.ai/v2/tokenplan/team",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        { id: "deepseek-v4-pro", name: "DeepSeek V4 Pro" },
        { id: "deepseek-v4-flash-0731", name: "DeepSeek V4 Flash 0731" },
        { id: "deepseek-v4.1-flash", name: "DeepSeek V4.1 Flash" },
        { id: "deepseek-v3.2", name: "DeepSeek V3.2" },
        { id: "glm-5.3", name: "GLM-5.3" },
        { id: "glm-5.1", name: "GLM-5.1" },
        { id: "glm-5", name: "GLM-5" },
        { id: "kimi-k2.6", name: "Kimi K2.6" },
      ],
    },
    suggestedDefaults: {
      model: { default: "deepseek-v4-pro", provider: "fluxa_tokenplan" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES.aigocode,
    settingsConfig: {
      name: "aigocode",
      base_url: "https://api.aigocode.dev",
      api_key: "",
      api_mode: "anthropic_messages",
      models: [
        {
          id: "claude-opus-5-5",
          name: "Claude Opus 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-sonnet-5-5",
          name: "Claude Sonnet 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-haiku-5-5",
          name: "Claude Haiku 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-fable-5-1",
          name: "Claude Fable 5.1",
          context_length: 1000000,
        },
      ],
    },
    suggestedDefaults: {
      model: { default: "claude-opus-5-5", provider: "aigocode" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES.qiniu,
    settingsConfig: {
      name: "qiniu",
      base_url: "https://api.qnaigc.com/v1",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        { id: "gpt-6-astra", name: "GPT-6 Astra", context_length: 272000 },
        { id: "moonshotai/kimi-k3", name: "Kimi K3", context_length: 1048576 },
        { id: "z-ai/glm-5.3", name: "GLM-5.3", context_length: 1048576 },
        {
          id: "z-ai/glm-5.3-flash",
          name: "GLM-5.3-Flash",
          context_length: 1048576,
        },
      ],
    },
    suggestedDefaults: {
      model: { default: "gpt-6-astra", provider: "qiniu" },
    },
  },
  {
    name: "AICoding",
    websiteUrl: "https://aicoding.inc",
    apiKeyUrl: "https://aicoding.inc/i/CCSWITCH",
    settingsConfig: {
      name: "aicoding",
      base_url: "https://api.aicoding.inc",
      api_key: "",
      api_mode: "anthropic_messages",
      models: [
        {
          id: "claude-opus-5-5",
          name: "Claude Opus 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-sonnet-5-5",
          name: "Claude Sonnet 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-haiku-5-5",
          name: "Claude Haiku 5.5",
          context_length: 1000000,
        },
      ],
    },
    category: "third_party",
    isPartner: true,
    partnerPromotionKey: "aicoding",
    icon: "aicoding",
    iconColor: "#000000",
    suggestedDefaults: {
      model: { default: "claude-opus-5-5", provider: "aicoding" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES.subrouter,
    settingsConfig: {
      name: "subrouter",
      base_url: "https://subrouter.ai/v1",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        { id: "gpt-6.1-sol", name: "GPT-6.1 Sol", context_length: 272000 },
        { id: "gpt-6-astra", name: "GPT-6 Astra", context_length: 272000 },
      ],
    },
    suggestedDefaults: {
      model: { default: "gpt-6.1-sol", provider: "subrouter" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES.shengsuanyun,
    settingsConfig: {
      name: "shengsuanyun",
      base_url: "https://router.shengsuanyun.com/api/v1",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        { id: "openai/gpt-6.1-sol", name: "GPT-6.1 Sol" },
        {
          id: "openai/gpt-6-astra",
          name: "GPT-6 Astra",
          context_length: 272000,
        },
        { id: "openai/gpt-6-luna", name: "GPT-6 Luna", context_length: 272000 },
      ],
    },
    suggestedDefaults: {
      model: { default: "openai/gpt-6.1-sol", provider: "shengsuanyun" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES["88api"],
    settingsConfig: {
      name: "88api",
      base_url: "https://api.88api.ai",
      api_key: "",
      api_mode: "anthropic_messages",
      models: [
        {
          id: "claude-opus-5-5",
          name: "Claude Opus 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-sonnet-5-5",
          name: "Claude Sonnet 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-haiku-5-5",
          name: "Claude Haiku 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-fable-5-1",
          name: "Claude Fable 5.1",
          context_length: 1000000,
        },
      ],
    },
    suggestedDefaults: {
      model: { default: "claude-opus-5-5", provider: "88api" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES.apikeyfun,
    settingsConfig: {
      name: "apikeyfun",
      base_url: "https://api.apikey.fan",
      api_key: "",
      api_mode: "anthropic_messages",
      models: [
        {
          id: "claude-opus-5-5",
          name: "Claude Opus 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-sonnet-5-5",
          name: "Claude Sonnet 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-haiku-5-5",
          name: "Claude Haiku 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-fable-5-1",
          name: "Claude Fable 5.1",
          context_length: 1000000,
        },
      ],
    },
    suggestedDefaults: {
      model: { default: "claude-opus-5-5", provider: "apikeyfun" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES["9527code"],
    settingsConfig: {
      name: "9527code",
      base_url: "https://9527.codes",
      api_key: "",
      api_mode: "anthropic_messages",
      models: [
        {
          id: "claude-opus-5-5",
          name: "Claude Opus 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-sonnet-5-5",
          name: "Claude Sonnet 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-haiku-5-5",
          name: "Claude Haiku 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-fable-5-1",
          name: "Claude Fable 5.1",
          context_length: 1000000,
        },
      ],
    },
    suggestedDefaults: {
      model: { default: "claude-opus-5-5", provider: "9527code" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES.code0,
    settingsConfig: {
      name: "code0",
      base_url: "https://code0.ai/v1",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        { id: "gpt-6.1-sol", name: "GPT-6.1 Sol" },
        { id: "gpt-6-astra", name: "GPT-6 Astra", context_length: 272000 },
      ],
    },
    suggestedDefaults: {
      model: { default: "gpt-6.1-sol", provider: "code0" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES.teamorouter,
    settingsConfig: {
      name: "teamorouter",
      base_url: "https://api.teamorouter.cn/v1",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        { id: "gpt-6.1-sol", name: "GPT-6.1 Sol" },
        { id: "gpt-6-astra", name: "GPT-6 Astra", context_length: 272000 },
        { id: "gpt-5.6-sol", name: "GPT-5.6 Sol" },
      ],
    },
    suggestedDefaults: {
      model: { default: "gpt-6.1-sol", provider: "teamorouter" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES.ppio,
    settingsConfig: {
      name: "ppio",
      base_url: "https://api.ppio.com/openai/v1",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        {
          id: "deepseek/deepseek-v4.1-flash",
          name: "DeepSeek V4.1 Flash",
          context_length: 1048576,
        },
        {
          id: "moonshotai/kimi-k3.1-pro",
          name: "Kimi K3.1 Pro",
          context_length: 1048576,
        },
        { id: "zai-org/glm-5.3", name: "GLM-5.3", context_length: 1000000 },
        {
          id: "deepseek/deepseek-v4-pro",
          name: "DeepSeek V4 Pro",
          context_length: 1000000,
        },
      ],
    },
    suggestedDefaults: {
      model: {
        default: "deepseek/deepseek-v4.1-flash",
        provider: "ppio",
      },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES.claudecn,
    settingsConfig: {
      name: "claudecn",
      base_url: "https://claudecn.top",
      api_key: "",
      api_mode: "anthropic_messages",
      models: [
        {
          id: "claude-opus-5-5",
          name: "Claude Opus 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-sonnet-5-5",
          name: "Claude Sonnet 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-haiku-5-5",
          name: "Claude Haiku 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-fable-5-1",
          name: "Claude Fable 5.1",
          context_length: 1000000,
        },
      ],
    },
    templateValues: {
      apiKey: {
        label: "API Key",
        placeholder: "",
        editorValue: "",
      },
    },
    suggestedDefaults: {
      model: { default: "claude-sonnet-5-5", provider: "claudecn" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES["火山-agent-plan"],
    settingsConfig: {
      name: "ark_agentplan",
      base_url: "https://ark.cn-beijing.volces.com/api/plan",
      api_key: "",
      api_mode: "anthropic_messages",
      models: [
        {
          id: "ark-code-latest",
          name: "Ark Code Latest",
        },
      ],
    },
    suggestedDefaults: {
      model: {
        default: "ark-code-latest",
        provider: "ark_agentplan",
      },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES["火山-coding-plan"],
    settingsConfig: {
      name: "ark_codingplan",
      base_url: "https://ark.cn-beijing.volces.com/api/coding",
      api_key: "",
      api_mode: "anthropic_messages",
      models: [
        {
          id: "ark-code-latest",
          name: "Ark Code Latest",
        },
      ],
    },
    suggestedDefaults: {
      model: {
        default: "ark-code-latest",
        provider: "ark_codingplan",
      },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES.byteplus,
    settingsConfig: {
      name: "byteplus",
      base_url: "https://ark.ap-southeast.bytepluses.com/api/coding",
      api_key: "",
      api_mode: "anthropic_messages",
      models: [
        {
          id: "ark-code-latest",
          name: "Ark Code Latest",
        },
      ],
    },
    suggestedDefaults: {
      model: {
        default: "ark-code-latest",
        provider: "byteplus",
      },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES["volcengine-doubao"],
    settingsConfig: {
      name: "doubao_seed",
      base_url: "https://ark.cn-beijing.volces.com/api/compatible",
      api_key: "",
      api_mode: "anthropic_messages",
      models: [
        {
          id: "doubao-seed-2-1-pro-260915",
          name: "Doubao Seed 2.1 Pro",
        },
      ],
    },
    suggestedDefaults: {
      model: {
        default: "doubao-seed-2-1-pro-260915",
        provider: "doubao_seed",
      },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES.siliconflow,
    settingsConfig: {
      name: "siliconflow",
      base_url: "https://api.siliconflow.cn/v1",
      api_key: "",
      api_mode: "chat_completions",
      // 国内站 M2.5 已于 2026-09-11 下线，与 Codex/OpenCode 同换 V4 Flash
      models: [
        {
          id: "deepseek-ai/DeepSeek-V4-Flash",
          name: "DeepSeek V4 Flash",
          context_length: 1048576,
        },
      ],
    },
    suggestedDefaults: {
      model: {
        default: "deepseek-ai/DeepSeek-V4-Flash",
        provider: "siliconflow",
      },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES["siliconflow-en"],
    settingsConfig: {
      name: "siliconflow_en",
      base_url: "https://api.siliconflow.com/v1",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        { id: "MiniMaxAI/MiniMax-M3", name: "MiniMax M3" },
        { id: "zai-org/GLM-5.3", name: "GLM-5.3", context_length: 1000000 },
        {
          id: "zai-org/GLM-5.3-Flash",
          name: "GLM-5.3 Flash",
          context_length: 1000000,
        },
        { id: "moonshotai/Kimi-K3", name: "Kimi K3", context_length: 1048576 },
        {
          id: "deepseek-ai/DeepSeek-V4-Pro",
          name: "DeepSeek V4 Pro",
          context_length: 1000000,
        },
      ],
    },
    suggestedDefaults: {
      model: {
        default: "MiniMaxAI/MiniMax-M3",
        provider: "siliconflow_en",
      },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES.a6api,
    settingsConfig: {
      name: "a6api",
      base_url: "https://api.a6api.com/v1",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        { id: "gpt-6.1-sol", name: "GPT-6.1 Sol" },
        { id: "gpt-6-astra", name: "GPT-6 Astra", context_length: 272000 },
      ],
    },
    suggestedDefaults: {
      model: { default: "gpt-6.1-sol", provider: "a6api" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES.compshare,
    settingsConfig: {
      name: "compshare",
      base_url: "https://api.modelverse.cn/v1",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        { id: "gpt-6-astra", name: "GPT-6 Astra", context_length: 272000 },
        { id: "kimi-k3", name: "Kimi K3", context_length: 1048576 },
        { id: "glm-5.3", name: "GLM-5.3", context_length: 1000000 },
        {
          id: "deepseek-v4.1-flash",
          name: "DeepSeek V4.1 Flash",
          context_length: 1000000,
        },
        { id: "qwen3.8-max", name: "Qwen3.8 Max", context_length: 1000000 },
        { id: "mimo-v2.6-pro", name: "MiMo V2.6 Pro", context_length: 1048576 },
      ],
    },
    suggestedDefaults: {
      model: { default: "gpt-6-astra", provider: "compshare" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES["compshare-coding-plan"],
    settingsConfig: {
      name: "compshare_coding",
      base_url: "https://cp.compshare.cn/v1",
      api_key: "",
      api_mode: "chat_completions",
      models: [{ id: "gpt-5.6-sol", name: "GPT-5.6 Sol" }],
    },
    suggestedDefaults: {
      model: { default: "gpt-5.6-sol", provider: "compshare_coding" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES.ccsub,
    settingsConfig: {
      name: "ccsub",
      base_url: "https://www.ccsub.net/v1",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        { id: "gpt-6.1-sol", name: "GPT-6.1 Sol", context_length: 272000 },
      ],
    },
    suggestedDefaults: {
      model: { default: "gpt-6.1-sol", provider: "ccsub" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES.sssaicode,
    settingsConfig: {
      name: "sssaicode",
      base_url: "https://node-hk.sssaicodeapi.com/api",
      api_key: "",
      api_mode: "anthropic_messages",
      models: [
        {
          id: "claude-opus-5-5",
          name: "Claude Opus 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-sonnet-5-5",
          name: "Claude Sonnet 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-haiku-5-5",
          name: "Claude Haiku 5.5",
          context_length: 1000000,
        },
      ],
    },
    suggestedDefaults: {
      model: { default: "claude-opus-5-5", provider: "sssaicode" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES.soleapi,
    settingsConfig: {
      name: "soleapi",
      base_url: "https://soleapi.com",
      api_key: "",
      api_mode: "anthropic_messages",
      models: [
        {
          id: "claude-opus-5-5",
          name: "Claude Opus 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-sonnet-5-5",
          name: "Claude Sonnet 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-haiku-5-5",
          name: "Claude Haiku 5.5",
          context_length: 1000000,
        },
      ],
    },
    suggestedDefaults: {
      model: { default: "claude-opus-5-5", provider: "soleapi" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES.micu,
    settingsConfig: {
      name: "micu",
      base_url: "https://www.micuapi.ai",
      api_key: "",
      api_mode: "anthropic_messages",
      models: [
        {
          id: "claude-opus-5-5",
          name: "Claude Opus 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-sonnet-5-5",
          name: "Claude Sonnet 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-haiku-5-5",
          name: "Claude Haiku 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-fable-5-1",
          name: "Claude Fable 5.1",
          context_length: 1000000,
        },
      ],
    },
    suggestedDefaults: {
      model: { default: "claude-opus-5-5", provider: "micu" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES.rightcode,
    settingsConfig: {
      name: "rightcode",
      base_url: "https://www.rightapi.ai/claude",
      api_key: "",
      api_mode: "anthropic_messages",
      models: [
        {
          id: "claude-opus-5-5",
          name: "Claude Opus 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-sonnet-5-5",
          name: "Claude Sonnet 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-haiku-5-5",
          name: "Claude Haiku 5.5",
          context_length: 1000000,
        },
      ],
    },
    suggestedDefaults: {
      model: { default: "claude-opus-5-5", provider: "rightcode" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES.cubence,
    settingsConfig: {
      name: "cubence",
      base_url: "https://api.cubence.com",
      api_key: "",
      api_mode: "anthropic_messages",
      models: [
        {
          id: "claude-opus-5-5",
          name: "Claude Opus 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-sonnet-5-5",
          name: "Claude Sonnet 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-haiku-5-5",
          name: "Claude Haiku 5.5",
          context_length: 1000000,
        },
      ],
    },
    suggestedDefaults: {
      model: { default: "claude-opus-5-5", provider: "cubence" },
    },
  },
  {
    name: "CrazyRouter",
    websiteUrl: "https://www.crazyrouter.com",
    apiKeyUrl: "https://www.crazyrouter.com/register?aff=OZcm&ref=cc-switch",
    settingsConfig: {
      name: "crazyrouter",
      base_url: "https://cn.crazyrouter.com",
      api_key: "",
      api_mode: "anthropic_messages",
      models: [
        {
          id: "claude-opus-5-5",
          name: "Claude Opus 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-sonnet-5-5",
          name: "Claude Sonnet 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-haiku-5-5",
          name: "Claude Haiku 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-fable-5-1",
          name: "Claude Fable 5.1",
          context_length: 1000000,
        },
      ],
    },
    category: "third_party",
    isPartner: true,
    partnerPromotionKey: "crazyrouter",
    icon: "crazyrouter",
    iconColor: "#000000",
    suggestedDefaults: {
      model: { default: "claude-opus-5-5", provider: "crazyrouter" },
    },
  },
  {
    name: "DMXAPI",
    websiteUrl: "https://www.dmxapi.cn",
    apiKeyUrl: "https://www.dmxapi.cn",
    settingsConfig: {
      name: "dmxapi",
      base_url: "https://www.dmxapi.cn/v1",
      api_key: "",
      api_mode: "chat_completions",
      models: [{ id: "gpt-6.1-sol", name: "GPT-6.1 Sol" }],
    },
    category: "aggregator",
    isPartner: true,
    partnerPromotionKey: "dmxapi",
    suggestedDefaults: {
      model: { default: "gpt-6.1-sol", provider: "dmxapi" },
    },
    icon: "dmxapi",
  },
  {
    ...PRESET_BRAND_IDENTITIES.sudocodechat,
    settingsConfig: {
      name: "sudocode",
      base_url: "https://api.sudocode.chat/v1",
      api_key: "",
      api_mode: "codex_responses",
      models: [
        { id: "gpt-6.1-sol", name: "GPT-6.1 Sol" },
        { id: "gpt-6-sol", name: "GPT-6 Sol", context_length: 272000 },
        { id: "gpt-6-luna", name: "GPT-6 Luna", context_length: 272000 },
        { id: "gpt-6-astra", name: "GPT-6 Astra", context_length: 272000 },
      ],
    },
    suggestedDefaults: {
      model: { default: "gpt-6.1-sol", provider: "sudocode" },
    },
  },
  {
    name: "SudoCode.us",
    family: "sudocode",
    websiteUrl: "https://sudocode.us",
    apiKeyUrl: "https://sudocode.us",
    settingsConfig: {
      name: "sudocode_us",
      base_url: "https://sudocode.us/v1",
      api_key: "",
      api_mode: "codex_responses",
      models: [
        { id: "gpt-6.1-sol", name: "GPT-6.1 Sol" },
        { id: "gpt-6-sol", name: "GPT-6 Sol", context_length: 272000 },
        { id: "gpt-6-astra", name: "GPT-6 Astra", context_length: 272000 },
      ],
    },
    category: "third_party",
    isPartner: true,
    icon: "sudocode-us",
    suggestedDefaults: {
      model: { default: "gpt-6.1-sol", provider: "sudocode_us" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES.xycai,
    settingsConfig: {
      name: "xycai",
      base_url: "https://apicdn.xycai.us/v1",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        { id: "gpt-6.1-sol", name: "GPT-6.1 Sol" },
        { id: "gpt-6-astra", name: "GPT-6 Astra", context_length: 272000 },
      ],
    },
    suggestedDefaults: {
      model: { default: "gpt-6.1-sol", provider: "xycai" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES.modelflare,
    settingsConfig: {
      name: "modelflare",
      // Chat Completions + /v1（docs.modelflare.dev/guides/endpoints，
      // 2026-10-15 核对）；阵容同 OpenCode
      base_url: "https://modelflare.dev/v1",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        { id: "gpt-6.1-sol", name: "GPT-6.1 Sol", context_length: 272000 },
        { id: "gpt-6-astra", name: "GPT-6 Astra", context_length: 272000 },
      ],
    },
    suggestedDefaults: {
      model: { default: "gpt-6.1-sol", provider: "modelflare" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES.alltokens,
    settingsConfig: {
      name: "alltokens",
      // Sub2API 网关：Key 绑定单个分组，须用 GPT 分组的 Key；GPT 账号池原生走
      // Responses（分组与模型见 alltokens.cc/api/v1/model-plaza，2026-10-17 核对）
      base_url: "https://alltokens.cc/v1",
      api_key: "",
      api_mode: "codex_responses",
      models: [
        { id: "gpt-6.1-sol", name: "GPT-6.1 Sol", context_length: 272000 },
        { id: "gpt-6-astra", name: "GPT-6 Astra", context_length: 272000 },
        { id: "gpt-6-luna", name: "GPT-6 Luna", context_length: 272000 },
      ],
    },
    suggestedDefaults: {
      model: { default: "gpt-6.1-sol", provider: "alltokens" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES["alltokens-si"],
    // 站点关闭了个人注册（/api/status 的 register_enabled=false），只能企业注册
    settingsConfig: {
      name: "alltokens_si",
      // new-api 网关，目录见 alltokens.si/api/pricing（2026-10-19 核对）：GPT 分组
      // 只声明 Chat Completions，窗口按中转口径封顶 272K
      base_url: "https://alltokens.si/v1",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        { id: "gpt-6.1-sol", name: "GPT-6.1 Sol", context_length: 272000 },
        { id: "gpt-6-astra", name: "GPT-6 Astra", context_length: 272000 },
      ],
    },
    suggestedDefaults: {
      model: { default: "gpt-6.1-sol", provider: "alltokens_si" },
    },
  },
  // ===== 非赞助商预设：应用内展示按显示名排序，此处文件顺序不影响展示 =====
  {
    ...PRESET_BRAND_IDENTITIES["tu-zi"],
    settingsConfig: {
      name: "tuzi",
      base_url: "https://api.tu-zi.com",
      api_key: "",
      api_mode: "anthropic_messages",
      models: [
        {
          id: "claude-opus-5-5",
          name: "Claude Opus 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-sonnet-5-5",
          name: "Claude Sonnet 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-haiku-5-5",
          name: "Claude Haiku 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-fable-5-1",
          name: "Claude Fable 5.1",
          context_length: 1000000,
        },
      ],
    },
    suggestedDefaults: {
      model: { default: "claude-opus-5-5", provider: "tuzi" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES.amux,
    settingsConfig: {
      name: "amux",
      base_url: "https://api.amux.ai/v1",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        { id: "gpt-6.1-sol", name: "GPT-6.1 Sol" },
        { id: "gpt-6-astra", name: "GPT-6 Astra", context_length: 272000 },
      ],
    },
    suggestedDefaults: {
      model: { default: "gpt-6.1-sol", provider: "amux" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES.atlascloud,
    settingsConfig: {
      name: "atlascloud",
      base_url: "https://api.atlascloud.ai/v1",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        { id: "zai-org/glm-5.3", name: "GLM-5.3", context_length: 1048576 },
        {
          id: "zai-org/glm-5.3-flash",
          name: "GLM-5.3 Flash",
          context_length: 1000000,
        },
        { id: "moonshotai/kimi-k3", name: "Kimi K3", context_length: 1048576 },
      ],
    },
    suggestedDefaults: {
      model: { default: "zai-org/glm-5.3", provider: "atlascloud" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES.soshow,
    settingsConfig: {
      name: "soshow",
      base_url: "https://maas.so-show.com",
      api_key: "",
      api_mode: "anthropic_messages",
      models: [
        { id: "claude-opus-5", name: "Claude Opus 5" },
        { id: "claude-sonnet-5", name: "Claude Sonnet 5" },
      ],
    },
    suggestedDefaults: {
      model: { default: "claude-opus-5", provider: "soshow" },
    },
  },
  {
    name: "OpenRouter",
    nameKey: "providerForm.presets.openrouter",
    websiteUrl: "https://openrouter.ai",
    apiKeyUrl: "https://openrouter.ai/keys",
    settingsConfig: {
      name: "openrouter",
      base_url: "https://openrouter.ai/api/v1",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        {
          id: "anthropic/claude-opus-5.5",
          name: "Claude Opus 5.5",
          context_length: 1000000,
        },
        {
          id: "anthropic/claude-sonnet-5.5",
          name: "Claude Sonnet 5.5",
          context_length: 1000000,
        },
        {
          id: "anthropic/claude-haiku-5.5",
          name: "Claude Haiku 5.5",
          context_length: 1000000,
        },
        {
          id: "openai/gpt-6.1-sol",
          name: "GPT-6.1 Sol",
          context_length: 1050000,
        },
        {
          id: "google/gemini-3.8-flash",
          name: "Gemini 3.8 Flash",
          context_length: 1048576,
        },
        {
          id: "anthropic/claude-fable-5.1",
          name: "Claude Fable 5.1",
          context_length: 1000000,
        },
        {
          id: "openai/gpt-6-astra",
          name: "GPT-6 Astra",
          context_length: 1050000,
        },
        {
          id: "google/gemini-4-argon",
          name: "Gemini 4 Argon",
          context_length: 1048576,
        },
      ],
    },
    category: "aggregator",
    icon: "openrouter",
    iconColor: "#6366F1",
    suggestedDefaults: {
      model: { default: "anthropic/claude-opus-5.5", provider: "openrouter" },
    },
  },
  {
    name: "DeepSeek",
    nameKey: "providerForm.presets.deepseek",
    websiteUrl: "https://platform.deepseek.com",
    apiKeyUrl: "https://platform.deepseek.com/api_keys",
    settingsConfig: {
      name: "deepseek",
      base_url: "https://api.deepseek.com",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        {
          id: "deepseek-v4-pro",
          name: "DeepSeek V4 Pro",
          context_length: 1000000,
        },
        {
          id: "deepseek-flash",
          name: "DeepSeek V4.1 Flash",
          context_length: 1000000,
        },
      ],
    },
    category: "cn_official",
    icon: "deepseek",
    iconColor: "#4D6BFE",
    suggestedDefaults: {
      model: { default: "deepseek-flash", provider: "deepseek" },
    },
  },
  {
    name: "Together AI",
    nameKey: "providerForm.presets.together",
    websiteUrl: "https://together.ai",
    apiKeyUrl: "https://api.together.ai/settings/api-keys",
    settingsConfig: {
      name: "together",
      base_url: "https://api.together.xyz/v1",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        {
          id: "Qwen/Qwen3.7-Max",
          name: "Qwen3.7 Max",
          context_length: 1000000,
        },
        {
          id: "deepseek-ai/DeepSeek-V4.1-Flash",
          name: "DeepSeek V4.1 Flash",
          context_length: 1048576,
        },
        { id: "moonshotai/Kimi-K3", name: "Kimi K3", context_length: 1048576 },
        { id: "zai-org/GLM-5.3", name: "GLM-5.3", context_length: 1048575 },
        {
          id: "zai-org/GLM-5.3-Flash",
          name: "GLM-5.3 Flash",
          context_length: 1048575,
        },
        {
          id: "MiniMaxAI/MiniMax-M3",
          name: "MiniMax M3",
          context_length: 524288,
        },
      ],
    },
    category: "aggregator",
    icon: "together",
    iconColor: "#0F6FFF",
    suggestedDefaults: {
      model: {
        default: "Qwen/Qwen3.7-Max",
        provider: "together",
      },
    },
  },
  {
    name: "Nous Research",
    websiteUrl: "https://nousresearch.com",
    apiKeyUrl: "https://portal.nousresearch.com/",
    settingsConfig: {
      name: "nous",
      base_url: "https://inference-api.nousresearch.com/v1",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        {
          id: "Hermes-4-405B",
          name: "Hermes 4 405B",
          context_length: 131072,
        },
        {
          id: "Hermes-4-70B",
          name: "Hermes 4 70B",
          context_length: 131072,
        },
      ],
    },
    isOfficial: true,
    category: "official",
    icon: "hermes",
    iconColor: "#7C3AED",
    suggestedDefaults: {
      model: { default: "Hermes-4-405B", provider: "nous" },
    },
  },

  // 字段映射：env.ANTHROPIC_BASE_URL → base_url；env.ANTHROPIC_AUTH_TOKEN → api_key；
  // apiFormat "anthropic"(默认) → api_mode "anthropic_messages"；
  // apiFormat "openai_chat" → api_mode "chat_completions"；
  // ANTHROPIC_MODEL / DEFAULT_HAIKU / SONNET / OPUS_MODEL 去重后塞进 models[]。
  {
    ...PRESET_BRAND_IDENTITIES["zhipu-glm"],
    settingsConfig: {
      name: "zhipu_glm",
      base_url: "https://open.bigmodel.cn/api/coding/paas/v4",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        { id: "glm-5.3", name: "GLM-5.3" },
        { id: "glm-5.3-flash", name: "GLM-5.3-Flash", context_length: 1048576 },
      ],
    },
    suggestedDefaults: {
      model: { default: "glm-5.3", provider: "zhipu_glm" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES["zhipu-glm-en"],
    settingsConfig: {
      name: "zhipu_glm_en",
      base_url: "https://api.z.ai/api/coding/paas/v4",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        { id: "glm-5.3", name: "GLM-5.3" },
        { id: "glm-5.3-flash", name: "GLM-5.3-Flash", context_length: 1048576 },
      ],
    },
    suggestedDefaults: {
      model: { default: "glm-5.3", provider: "zhipu_glm_en" },
    },
  },
  {
    // 腾讯云 Token Plan 个人版（1823/130060，2026-08-21 版）：通用 + Hy 两
    // 系列共用同一端点与 API Key，模型合并两系列；Auto 智能路由调用 ID 是
    // tc-code-latest。官方未发 Hermes 专属接入页，按工具无关的 /plan/v3
    // OpenAI Chat 端点收录（与市场线 1300/80643 的 chat_completions 模式
    // 一致）。kimi-k2.5 官方 2026-08-31 下线不收；minimax-m2.5 真 Key
    // 实测可用（2026-08-31）照实收
    name: "Tencent Token Plan",
    family: "tencent",
    planKey: "tokenPlan",
    regionKey: "cn",
    websiteUrl: "https://cloud.tencent.com/product/tokenhub",
    apiKeyUrl: "https://console.cloud.tencent.com/tokenhub/tokenplan",
    settingsConfig: {
      name: "tencent_token_plan",
      base_url: "https://api.lkeap.cloud.tencent.com/plan/v3",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        { id: "tc-code-latest", name: "Auto" },
        { id: "deepseek-v4-flash-202605", name: "DeepSeek V4 Flash" },
        { id: "deepseek-v4-pro-202606", name: "DeepSeek V4 Pro" },
        { id: "minimax-m2.7", name: "MiniMax M2.7" },
        { id: "glm-5", name: "GLM-5" },
        { id: "glm-5.1", name: "GLM-5.1" },
        { id: "glm-5.2", name: "GLM-5.2" },
        { id: "hy3", name: "Hy3" },
        { id: "hy3-preview", name: "Hy3 Preview" },
      ],
    },
    category: "cn_official",
    icon: "tencent",
    iconColor: "#0052D9",
    suggestedDefaults: {
      model: { default: "tc-code-latest", provider: "tencent_token_plan" },
    },
  },
  {
    // 国际站（新加坡地域）个人版（intl 1300/81315，2026-08-20 版）：Auto
    // 调用 ID 是 auto（≠国内 tc-code-latest），阵容不同（无 GLM-5/5.1/
    // Hy3）。端点用国际站文档钦定的 tencentcloudmaas.com 域；Key 按站独立
    name: "Tencent Token Plan (Intl)",
    family: "tencent",
    planKey: "tokenPlan",
    regionKey: "intl",
    websiteUrl: "https://www.tencentcloud.com/products/tokenhub",
    apiKeyUrl: "https://console.tencentcloud.com/tokenhub/tokenplan",
    settingsConfig: {
      name: "tencent_token_plan_intl",
      base_url: "https://tokenhub-intl.tencentcloudmaas.com/plan/v3",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        { id: "auto", name: "Auto" },
        { id: "glm-5.2", name: "GLM-5.2" },
        { id: "kimi-k2.6", name: "Kimi K2.6" },
        { id: "deepseek-v4-pro-202606", name: "DeepSeek V4 Pro" },
        { id: "deepseek-v4-flash-202605", name: "DeepSeek V4 Flash" },
        { id: "minimax-m3", name: "MiniMax M3" },
      ],
    },
    category: "cn_official",
    icon: "tencent",
    iconColor: "#0052D9",
    suggestedDefaults: {
      model: { default: "auto", provider: "tencent_token_plan_intl" },
    },
  },
  {
    // Token Plan 企业版专业套餐（1823/130659，2026-08-25 版，广州地域）：
    // kimi-k2.5 官方 2026-08-31 下线不收；minimax-m2.5 文档已除名但真 Key
    // 实测可用（2026-08-31），照实收录
    name: "Tencent Token Plan Enterprise Pro",
    family: "tencent",
    planKey: "enterprisePro",
    regionKey: "cn",
    websiteUrl: "https://cloud.tencent.com/product/tokenhub",
    apiKeyUrl: "https://console.cloud.tencent.com/tokenhub/tokenplan-e",
    settingsConfig: {
      name: "tencent_token_plan_enterprise_pro",
      base_url: "https://tokenhub.tencentmaas.com/plan/v3",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        { id: "auto", name: "Auto" },
        { id: "glm-5.3", name: "GLM-5.3" },
        { id: "glm-5.2", name: "GLM-5.2" },
        { id: "glm-5", name: "GLM-5" },
        { id: "glm-5.1", name: "GLM-5.1" },
        { id: "glm-5-turbo", name: "GLM-5 Turbo" },
        { id: "kimi-k2.7-code", name: "Kimi K2.7 Code" },
        { id: "kimi-k2.7-code-highspeed", name: "Kimi K2.7 Code HighSpeed" },
        { id: "kimi-k2.6", name: "Kimi K2.6" },
        { id: "minimax-m3", name: "MiniMax M3" },
        { id: "deepseek-v4-flash", name: "DeepSeek V4 Flash" },
        { id: "deepseek-v4-pro", name: "DeepSeek V4 Pro" },
        { id: "deepseek-v4-flash-0731", name: "DeepSeek V4 Flash 0731 GA" },
        { id: "deepseek-v4-pro-0813", name: "DeepSeek V4 Pro 0813 GA" },
        { id: "deepseek-v4-flash-202605", name: "DeepSeek V4 Flash Official" },
        { id: "deepseek-v4-pro-202606", name: "DeepSeek V4 Pro Official" },
      ],
    },
    category: "cn_official",
    icon: "tencent",
    iconColor: "#0052D9",
    suggestedDefaults: {
      model: {
        default: "auto",
        provider: "tencent_token_plan_enterprise_pro",
      },
    },
  },
  {
    // 国际站企业版专业套餐（intl 1300/81489，2026-08-26 版，新加坡地域）：
    // 阵容为广州地域子集（无 GLM-5/5.1/5-Turbo、Kimi-K2.6、MiniMax-M2.7）
    name: "Tencent Token Plan Enterprise Pro (Intl)",
    family: "tencent",
    planKey: "enterprisePro",
    regionKey: "intl",
    websiteUrl: "https://www.tencentcloud.com/products/tokenhub",
    apiKeyUrl: "https://console.tencentcloud.com/tokenhub/tokenplan-e",
    settingsConfig: {
      name: "tencent_token_plan_enterprise_pro_intl",
      base_url: "https://tokenhub-intl.tencentcloudmaas.com/plan/v3",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        { id: "auto", name: "Auto" },
        { id: "glm-5.3", name: "GLM-5.3" },
        { id: "glm-5.2", name: "GLM-5.2" },
        { id: "minimax-m3", name: "MiniMax M3" },
        { id: "kimi-k2.7-code", name: "Kimi K2.7 Code" },
        { id: "kimi-k2.7-code-highspeed", name: "Kimi K2.7 Code HighSpeed" },
        { id: "deepseek-v4-flash", name: "DeepSeek V4 Flash" },
        { id: "deepseek-v4-pro", name: "DeepSeek V4 Pro" },
        { id: "deepseek-v4-flash-0731", name: "DeepSeek V4 Flash 0731 GA" },
        { id: "deepseek-v4-pro-0813", name: "DeepSeek V4 Pro 0813 GA" },
        { id: "deepseek-v4-flash-202605", name: "DeepSeek V4 Flash Official" },
        { id: "deepseek-v4-pro-202606", name: "DeepSeek V4 Pro Official" },
      ],
    },
    category: "cn_official",
    icon: "tencent",
    iconColor: "#0052D9",
    suggestedDefaults: {
      model: {
        default: "auto",
        provider: "tencent_token_plan_enterprise_pro_intl",
      },
    },
  },
  {
    // Token Plan 企业版轻享套餐（1823/131173，2026-08-28 版）：仅 Auto 模型
    name: "Tencent Token Plan Enterprise Lite",
    family: "tencent",
    planKey: "enterpriseLite",
    regionKey: "cn",
    websiteUrl: "https://cloud.tencent.com/product/tokenhub",
    apiKeyUrl: "https://console.cloud.tencent.com/tokenhub/tokenplan-e",
    settingsConfig: {
      name: "tencent_token_plan_enterprise_lite",
      base_url: "https://tokenhub.tencentmaas.com/plan/v3",
      api_key: "",
      api_mode: "chat_completions",
      models: [{ id: "auto", name: "Auto" }],
    },
    category: "cn_official",
    icon: "tencent",
    iconColor: "#0052D9",
    suggestedDefaults: {
      model: {
        default: "auto",
        provider: "tencent_token_plan_enterprise_lite",
      },
    },
  },
  {
    // 国际站企业版轻享套餐（intl 1300/81490）：新加坡地域（资源调度范围
    // Global），仅 Auto 模型
    name: "Tencent Token Plan Enterprise Lite (Intl)",
    family: "tencent",
    planKey: "enterpriseLite",
    regionKey: "intl",
    websiteUrl: "https://www.tencentcloud.com/products/tokenhub",
    apiKeyUrl: "https://console.tencentcloud.com/tokenhub/tokenplan-e",
    settingsConfig: {
      name: "tencent_token_plan_enterprise_lite_intl",
      base_url: "https://tokenhub-intl.tencentcloudmaas.com/plan/v3",
      api_key: "",
      api_mode: "chat_completions",
      models: [{ id: "auto", name: "Auto" }],
    },
    category: "cn_official",
    icon: "tencent",
    iconColor: "#0052D9",
    suggestedDefaults: {
      model: {
        default: "auto",
        provider: "tencent_token_plan_enterprise_lite_intl",
      },
    },
  },
  {
    // 千帆 Token Plan 个人版（2026-07-13 起替代 Coding Plan 发售）：官方
    // Hermes 接入页确认 /v2/tokenplan/personal、默认 deepseek-v4-pro（其
    // api_mode 写 "openai_messages"，本仓 OpenAI Chat 端点惯例统一映射为
    // chat_completions）；阵容=Token Plan 主文档 2026-08-14 版六模型
    name: "Baidu Qianfan Token Plan",
    websiteUrl: "https://cloud.baidu.com/product/codingplan.html",
    apiKeyUrl: "https://console.bce.baidu.com/qianfan/resource/token-plan",
    settingsConfig: {
      name: "qianfan_tokenplan",
      base_url: "https://qianfan.baidubce.com/v2/tokenplan/personal",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        { id: "deepseek-v4-pro", name: "DeepSeek V4 Pro" },
        { id: "deepseek-v4-flash", name: "DeepSeek V4 Flash" },
        { id: "deepseek-v4-flash-0731", name: "DeepSeek V4 Flash 0731" },
        { id: "glm-5.2", name: "GLM-5.2" },
        { id: "glm-5.1", name: "GLM-5.1" },
        { id: "kimi-k2.6", name: "Kimi K2.6" },
      ],
    },
    category: "cn_official",
    icon: "baidu",
    iconColor: "#2932E1",
    suggestedDefaults: {
      model: { default: "deepseek-v4-pro", provider: "qianfan_tokenplan" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES["千问ai平台"],
    settingsConfig: {
      name: "qianwenai",
      base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        { id: "qwen3.8-max", name: "Qwen3.8 Max", context_length: 983616 },
        { id: "qwen3.8-flash", name: "Qwen3.8 Flash", context_length: 983616 },
      ],
    },
    suggestedDefaults: {
      model: { default: "qwen3.8-max", provider: "qianwenai" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES["千问ai平台-coding-plan"],
    settingsConfig: {
      name: "qianwenai_coding_plan",
      base_url: "https://coding.dashscope.aliyuncs.com/apps/anthropic",
      api_key: "",
      api_mode: "anthropic_messages",
      models: [
        { id: "qwen3-coder-plus", name: "Qwen3 Coder Plus" },
        { id: "qwen3-max", name: "Qwen3 Max" },
      ],
    },
    suggestedDefaults: {
      model: {
        default: "qwen3-coder-plus",
        provider: "qianwenai_coding_plan",
      },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES["千问ai平台-token-plan"],
    settingsConfig: {
      name: "qianwenai_token_plan",
      base_url:
        "https://token-plan.cn-beijing.maas.aliyuncs.com/apps/anthropic",
      api_key: "",
      api_mode: "anthropic_messages",
      models: [
        { id: "qwen3.8-max", name: "Qwen3.8 Max", context_length: 983616 },
        { id: "qwen3.8-flash", name: "Qwen3.8 Flash", context_length: 983616 },
      ],
    },
    suggestedDefaults: {
      model: { default: "qwen3.8-max", provider: "qianwenai_token_plan" },
    },
  },
  // ===== QwenCloud（DashScope 国际站）=====
  // 与上面国内条目是两套独立站点：域名、控制台、密钥互不通用。
  // QwenCloud 与 Token Plan 都用 anthropic_messages，地址不带 /v1（与官方
  // hermes 文档一致，这点和 OpenCode / OpenClaw 不同，勿互相照搬）。
  {
    ...PRESET_BRAND_IDENTITIES.qwencloud,
    settingsConfig: {
      name: "qwencloud",
      base_url: "https://dashscope-intl.aliyuncs.com/apps/anthropic",
      api_key: "",
      api_mode: "anthropic_messages",
      models: [
        { id: "qwen3.8-max", name: "Qwen3.8 Max", context_length: 983616 },
        { id: "qwen3.8-flash", name: "Qwen3.8 Flash", context_length: 983616 },
        { id: "qwen3.7-max", name: "Qwen3.7 Max", context_length: 1000000 },
      ],
    },
    suggestedDefaults: {
      model: { default: "qwen3.8-max", provider: "qwencloud" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES["qwencloud-for-coding"],
    settingsConfig: {
      name: "qwencloud_coding",
      base_url: "https://coding-intl.dashscope.aliyuncs.com/apps/anthropic",
      api_key: "",
      api_mode: "anthropic_messages",
      models: [
        { id: "qwen3.7-plus", name: "Qwen3.7 Plus", context_length: 1000000 },
        { id: "qwen3.6-plus", name: "Qwen3.6 Plus", context_length: 1000000 },
        {
          id: "qwen3-coder-plus",
          name: "Qwen3 Coder Plus",
          context_length: 131072,
        },
      ],
    },
    suggestedDefaults: {
      model: { default: "qwen3.7-plus", provider: "qwencloud_coding" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES["qwencloud-token-plan"],
    settingsConfig: {
      name: "qwencloud_token_plan",
      base_url:
        "https://token-plan.ap-southeast-1.maas.aliyuncs.com/apps/anthropic",
      api_key: "",
      api_mode: "anthropic_messages",
      models: [
        { id: "qwen3.8-max", name: "Qwen3.8 Max", context_length: 983616 },
        { id: "qwen3.8-flash", name: "Qwen3.8 Flash", context_length: 983616 },
        { id: "qwen3.7-max", name: "Qwen3.7 Max", context_length: 1000000 },
        { id: "qwen3.7-plus", name: "Qwen3.7 Plus", context_length: 1000000 },
        { id: "qwen3.6-plus", name: "Qwen3.6 Plus", context_length: 1000000 },
        { id: "qwen3.6-flash", name: "Qwen3.6 Flash", context_length: 1000000 },
      ],
    },
    suggestedDefaults: {
      model: { default: "qwen3.8-max", provider: "qwencloud_token_plan" },
    },
  },
  {
    name: "StepFun",
    websiteUrl: "https://platform.stepfun.ai",
    apiKeyUrl: "https://platform.stepfun.ai/interface-key",
    settingsConfig: {
      name: "stepfun",
      base_url: "https://api.stepfun.ai/v1",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        { id: "step-3.5-flash", name: "Step 3.5 Flash" },
        {
          id: "step-3.7-flash",
          name: "Step 3.7 Flash",
          context_length: 256000,
        },
        {
          id: "step-5-preview",
          name: "Step 5 Preview",
          context_length: 1000000,
        },
      ],
    },
    category: "cn_official",
    icon: "stepfun",
    iconColor: "#005AFF",
    suggestedDefaults: {
      model: { default: "step-3.5-flash", provider: "stepfun" },
    },
  },
  {
    name: "ModelScope",
    websiteUrl: "https://modelscope.cn",
    settingsConfig: {
      name: "modelscope",
      base_url: "https://api-inference.modelscope.cn/v1",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        { id: "ZhipuAI/GLM-5.2", name: "ZhipuAI / GLM-5.2" },
        {
          id: "deepseek-ai/DeepSeek-V4.1-Flash",
          name: "DeepSeek V4.1 Flash",
          context_length: 1048576,
        },
        {
          id: "MiniMax/MiniMax-M3",
          name: "MiniMax M3",
          context_length: 1000000,
        },
      ],
    },
    category: "aggregator",
    icon: "modelscope",
    iconColor: "#624AFF",
    suggestedDefaults: {
      model: { default: "ZhipuAI/GLM-5.2", provider: "modelscope" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES.streamlake,
    settingsConfig: {
      name: "streamlake",
      base_url:
        "https://wanqing.streamlakeapi.com/api/gateway/v1/endpoints/kat-coder-pro-v2.5/claude-code-proxy",
      api_key: "",
      api_mode: "anthropic_messages",
      models: [{ id: "kat-coder-pro-v2.5", name: "KAT-Coder Pro V2.5" }],
    },
    suggestedDefaults: {
      model: { default: "kat-coder-pro-v2.5", provider: "streamlake" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES["streamlake-en"],
    settingsConfig: {
      name: "streamlake_en",
      base_url:
        "https://vanchin.streamlake.ai/api/gateway/v1/endpoints/kat-coder-pro-v2.5/claude-code-proxy",
      api_key: "",
      api_mode: "anthropic_messages",
      models: [{ id: "kat-coder-pro-v2.5", name: "KAT-Coder Pro V2.5" }],
    },
    suggestedDefaults: {
      model: { default: "kat-coder-pro-v2.5", provider: "streamlake_en" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES.longcat,
    settingsConfig: {
      name: "longcat",
      base_url: "https://api.longcat.chat/openai/v1",
      api_key: "",
      api_mode: "chat_completions",
      models: [{ id: "LongCat-2.0", name: "LongCat 2.0" }],
    },
    suggestedDefaults: {
      model: { default: "LongCat-2.0", provider: "longcat" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES.minimax,
    // `minimax` / `minimax-cn` are Hermes built-ins that shadow custom_providers
    providerKey: "cc-switch-minimax",
    settingsConfig: {
      name: "cc-switch-minimax",
      base_url: "https://api.minimax.cn/v1",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        {
          id: "MiniMax-M3.1-Flash",
          name: "MiniMax M3.1 Flash",
          supports_vision: true,
        },
        { id: "MiniMax-M3", name: "MiniMax M3" },
      ],
      // M3.1 can't disable thinking; Hermes' title request sends reasoning_effort:none
      title_generation_model: "MiniMax-M3",
    },
    theme: { backgroundColor: "#f64551", textColor: "#FFFFFF" },
    suggestedDefaults: {
      model: { default: "MiniMax-M3.1-Flash", provider: "cc-switch-minimax" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES["minimax-en"],
    providerKey: "cc-switch-minimax-en",
    settingsConfig: {
      name: "cc-switch-minimax-en",
      base_url: "https://api.minimax.io/v1",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        {
          id: "MiniMax-M3.1-Flash",
          name: "MiniMax M3.1 Flash",
          supports_vision: true,
        },
        { id: "MiniMax-M3", name: "MiniMax M3" },
      ],
      // M3.1 can't disable thinking; Hermes' title request sends reasoning_effort:none
      title_generation_model: "MiniMax-M3",
    },
    theme: { backgroundColor: "#f64551", textColor: "#FFFFFF" },
    suggestedDefaults: {
      model: {
        default: "MiniMax-M3.1-Flash",
        provider: "cc-switch-minimax-en",
      },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES.bailing,
    settingsConfig: {
      name: "bailing",
      base_url: "https://api.ant-ling.com/anthropic",
      api_key: "",
      api_mode: "anthropic_messages",
      models: [{ id: "Ling-2.6-1T", name: "Ling 2.6 1T" }],
    },
    suggestedDefaults: {
      model: { default: "Ling-2.6-1T", provider: "bailing" },
    },
  },
  {
    name: "AiHubMix",
    websiteUrl: "https://aihubmix.com",
    apiKeyUrl: "https://aihubmix.com",
    settingsConfig: {
      name: "aihubmix",
      base_url: "https://aihubmix.com/v1",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        { id: "gpt-6.1-sol", name: "GPT-6.1 Sol" },
        { id: "gpt-6-astra", name: "GPT-6 Astra", context_length: 272000 },
      ],
    },
    category: "aggregator",
    icon: "aihubmix",
    iconColor: "#006FFB",
    suggestedDefaults: {
      model: { default: "gpt-6.1-sol", provider: "aihubmix" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES.cherryin,
    settingsConfig: {
      name: "cherryin",
      base_url: "https://open.cherryin.net",
      api_key: "",
      api_mode: "anthropic_messages",
      models: [
        {
          id: "anthropic/claude-opus-5.5",
          name: "Claude Opus 5.5",
          context_length: 1000000,
        },
        {
          id: "anthropic/claude-sonnet-5.5",
          name: "Claude Sonnet 5.5",
          context_length: 1000000,
        },
        {
          id: "anthropic/claude-fable-5.1",
          name: "Claude Fable 5.1",
          context_length: 1000000,
        },
      ],
    },
    suggestedDefaults: {
      model: { default: "anthropic/claude-opus-5.5", provider: "cherryin" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES["e-flowcode"],
    settingsConfig: {
      name: "eflowcode",
      base_url: "https://e-flowcode.cc",
      api_key: "",
      api_mode: "anthropic_messages",
      models: [
        {
          id: "claude-opus-5-5",
          name: "Claude Opus 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-sonnet-5-5",
          name: "Claude Sonnet 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-haiku-5-5",
          name: "Claude Haiku 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-fable-5-1",
          name: "Claude Fable 5.1",
          context_length: 1000000,
        },
      ],
    },
    suggestedDefaults: {
      model: { default: "claude-opus-5-5", provider: "eflowcode" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES.therouter,
    settingsConfig: {
      name: "therouter",
      base_url: "https://api.therouter.ai/v1",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        { id: "openai/gpt-6.1-sol", name: "GPT-6.1 Sol" },
        { id: "openai/gpt-6-luna", name: "GPT-6 Luna" },
      ],
    },
    suggestedDefaults: {
      model: {
        default: "openai/gpt-6.1-sol",
        provider: "therouter",
      },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES.baseten,
    settingsConfig: {
      name: "baseten",
      // OpenAI Chat 端点 /v1/chat/completions（docs.baseten.co/development/
      // model-apis/overview，2026-10-14 核对）；窗口取同页模型表
      base_url: "https://inference.baseten.co/v1",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        { id: "zai-org/GLM-5.3", name: "GLM-5.3", context_length: 1048576 },
        { id: "moonshotai/Kimi-K3", name: "Kimi K3", context_length: 1048576 },
        {
          id: "zai-org/GLM-5.3-Flash",
          name: "GLM-5.3 Flash",
          context_length: 1048576,
        },
        {
          id: "zai-org/GLM-5.3-Fast",
          name: "GLM-5.3 Fast",
          context_length: 1048576,
        },
        {
          id: "deepseek-ai/DeepSeek-V4.1-Flash-1017",
          name: "DeepSeek V4.1 Flash 1017",
          context_length: 1048576,
        },
      ],
    },
    suggestedDefaults: {
      model: { default: "zai-org/GLM-5.3", provider: "baseten" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES["novita-ai"],
    settingsConfig: {
      name: "novita",
      base_url: "https://api.novita.ai/v3/openai",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        { id: "zai-org/glm-5.3", name: "GLM-5.3", context_length: 1048576 },
        {
          id: "zai-org/glm-5.3-flash",
          name: "GLM-5.3-Flash",
          context_length: 1048576,
        },
        {
          id: "moonshotai/kimi-k3.1-pro",
          name: "Kimi K3.1 Pro",
          context_length: 1048576,
        },
        {
          id: "deepseek/deepseek-v4.1-flash",
          name: "DeepSeek V4.1 Flash",
          context_length: 1048576,
        },
        {
          id: "minimax/minimax-m3.1-flash",
          name: "MiniMax M3.1 Flash",
          context_length: 1000000,
        },
        {
          id: "qwen/qwen3.8-max",
          name: "Qwen3.8 Max",
          context_length: 1000000,
        },
        {
          id: "xiaomimimo/mimo-v2.6-pro",
          name: "MiMo V2.6 Pro",
          context_length: 1048576,
        },
      ],
    },
    suggestedDefaults: {
      model: { default: "zai-org/glm-5.3", provider: "novita" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES.nvidia,
    settingsConfig: {
      name: "nvidia",
      base_url: "https://integrate.api.nvidia.com",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        {
          id: "moonshotai/kimi-k2.6",
          name: "Moonshot Kimi K2.6",
          context_length: 262144,
        },
        { id: "z-ai/glm-5.3", name: "GLM-5.3", context_length: 1048576 },
        {
          id: "z-ai/glm-5.3-flash",
          name: "GLM-5.3-Flash",
          context_length: 1048576,
        },
      ],
    },
    suggestedDefaults: {
      model: { default: "moonshotai/kimi-k2.6", provider: "nvidia" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES["ollama-cloud"],
    settingsConfig: {
      // Hermes 自带内置 provider ollama-cloud（读 OLLAMA_API_KEY），用下划线
      // 名避免与之同名，确保走这里的 custom_providers 与 api_key
      name: "ollama_cloud",
      // 云端 OpenAI 兼容 base（docs.ollama.com/api/openai-compatibility，
      // 2026-10-14 核对）；窗口取 https://ollama.com/api/show
      base_url: "https://ollama.com/v1",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        { id: "glm-5.3", name: "GLM-5.3", context_length: 1048576 },
        {
          id: "glm-5.3-flash",
          name: "GLM-5.3-Flash",
          context_length: 1048576,
        },
        { id: "kimi-k3", name: "Kimi K3", context_length: 1048576 },
        {
          id: "deepseek-v4-pro:0813",
          name: "DeepSeek V4 Pro",
          context_length: 1048576,
        },
        {
          id: "deepseek-v4.1-flash",
          name: "DeepSeek V4.1 Flash",
          context_length: 1048576,
        },
        { id: "minimax-m3", name: "MiniMax M3", context_length: 512000 },
      ],
    },
    suggestedDefaults: {
      model: { default: "glm-5.3", provider: "ollama_cloud" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES.ollama,
    settingsConfig: {
      // Hermes 里 ollama / local 都是指向 custom 的别名（hermes_cli/auth.py），
      // 照 ollama_cloud 先例换名避开
      name: "ollama-local",
      // Hermes 官方本机写法：base http://localhost:11434/v1、Key 留空
      //（hermes-agent.nousresearch.com/docs/integrations/providers，2026-10-14
      // 核对）。Hermes 启动时拒绝小于 64000 的窗口：这里填模型原生值，服务端
      // 实际窗口也要至少 64k（OLLAMA_CONTEXT_LENGTH 或 model.ollama_num_ctx）。
      // 模型换成自己已下载的
      base_url: "http://localhost:11434/v1",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        { id: "qwen3.5", name: "Qwen3.5", context_length: 262144 },
        { id: "gpt-oss:20b", name: "GPT-OSS 20B", context_length: 131072 },
      ],
    },
    suggestedDefaults: {
      model: { default: "qwen3.5", provider: "ollama-local" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES["lm-studio"],
    settingsConfig: {
      // Hermes 内置 provider lmstudio，且 lm-studio / lm_studio 都是它的别名
      //（hermes_cli/auth.py），照 ollama-local 先例换名避开
      name: "lmstudio-local",
      // base http://localhost:1234/v1、默认不鉴权，Key 留空（开启鉴权后填 API
      // Token；hermes-agent.nousresearch.com/docs/integrations/providers，
      // 2026-10-14 核对）。Hermes 启动时拒绝小于 64000 的窗口：这里填模型原生
      // 值，LM Studio 加载模型时的 context length 也要至少 64K（UI 设置或
      // lms load --context-length），加载窗口更小时同步调小。模型换成自己已
      // 下载的，或用「获取模型」从 /v1/models 拉取
      base_url: "http://localhost:1234/v1",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        {
          id: "openai/gpt-oss-20b",
          name: "GPT-OSS 20B",
          context_length: 131072,
        },
      ],
    },
    suggestedDefaults: {
      model: { default: "openai/gpt-oss-20b", provider: "lmstudio-local" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES.llamacpp,
    settingsConfig: {
      // Hermes 里 llamacpp / llama.cpp / llama-cpp 都是指向 custom 的别名
      //（hermes_cli/auth.py、providers.py），照 lmstudio-local 先例换名避开
      name: "llamacpp-local",
      // 本机 llama-server 的 OpenAI Chat base（github.com/ggml-org/llama.cpp/
      // blob/master/tools/server/README.md、hermes-agent.nousresearch.com/docs/
      // integrations/providers，2026-10-14 核对）。默认端口 b11521（PR #30159）
      // 起为 9931，v0.6.0 及更早、Homebrew 与 Docker 仍是 8080；只监听 IPv4 的
      // 127.0.0.1。不带 --api-key 时不校验，Key 留空（带了就填那个值）。Hermes
      // 启动时拒绝小于 64000 的窗口：这里填模型原生值，llama-server 的 -c 也要
      // 至少 64K（--fit 可能自动调小），实际窗口更小时同步调小。默认模型取
      // ggml-org 官方 GGUF 的 Qwen3-Coder 30B A3B，换成自己已下载的，或用
      // 「获取模型」从 /v1/models 拉取
      base_url: "http://127.0.0.1:9931/v1",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        {
          id: "ggml-org/Qwen3-Coder-30B-A3B-Instruct-Q8_0-GGUF",
          name: "Qwen3-Coder 30B A3B",
          context_length: 262144,
        },
      ],
    },
    suggestedDefaults: {
      model: {
        default: "ggml-org/Qwen3-Coder-30B-A3B-Instruct-Q8_0-GGUF",
        provider: "llamacpp-local",
      },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES.vllm,
    settingsConfig: {
      // Hermes 里 vllm / local 都是指向 custom 的别名（hermes_cli/providers.py），
      // 照 llamacpp-local 先例换名避开
      name: "vllm-local",
      // 本机 vllm serve 的 OpenAI Chat base（docs.vllm.ai/en/latest/serving/
      // online_serving/openai_compatible_server、hermes-agent.nousresearch.com/
      // docs/integrations/providers，2026-10-14 核对）。默认端口 8000，不带
      // --host 时监听全部 IPv4 网卡，建议加 --host 127.0.0.1；工具调用要启动时加
      // --enable-auto-tool-choice --tool-call-parser qwen3_coder。不带 --api-key
      // 时不校验，Key 留空（Hermes 会发占位值 no-key-required；带了就填那个值）。
      // Hermes 启动时拒绝小于 64000 的窗口：这里填模型原生值，--max-model-len
      // 也要至少 64K，实际窗口更小时同步调小。默认模型取 vLLM Codex 接入页的
      // 示例 Qwen/Qwen3.6-27B，须与 /v1/models 的 id 一致，换成自己已下载的，
      // 或用「获取模型」从 /v1/models 拉取
      base_url: "http://localhost:8000/v1",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        {
          id: "Qwen/Qwen3.6-27B",
          name: "Qwen3.6 27B",
          context_length: 262144,
        },
      ],
    },
    suggestedDefaults: {
      model: { default: "Qwen/Qwen3.6-27B", provider: "vllm-local" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES["meta-model-api"],
    settingsConfig: {
      name: "meta",
      // OpenAI Chat 端点 /v1/chat/completions（dev.meta.ai/docs/coding-agents.md，
      // 2026-10-14 核对），Bearer 鉴权；窗口取 dev.meta.ai/docs/models.md
      base_url: "https://api.meta.ai/v1",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        {
          id: "muse-wonder-1.0",
          name: "Muse Wonder 1.0",
          context_length: 1048576,
        },
        {
          id: "muse-spark-1.3",
          name: "Muse Spark 1.3",
          context_length: 1048576,
        },
      ],
    },
    suggestedDefaults: {
      model: { default: "muse-wonder-1.0", provider: "meta" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES.deepinfra,
    settingsConfig: {
      name: "deepinfra",
      // 官方 Hermes 接入文档给旧版 Hermes 的自定义端点即 /v1/openai Chat
      //（docs.deepinfra.com/integrations/hermes-agent.md，2026-10-14 核对）。
      // Hermes 已内置 deepinfra，同名写法照 openrouter / deepseek 先例；窗口取
      // https://api.deepinfra.com/models/<id> 同日元数据
      base_url: "https://api.deepinfra.com/v1/openai",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        {
          id: "deepseek-ai/DeepSeek-V4.1-Flash-1017",
          name: "DeepSeek V4.1 Flash 1017",
          context_length: 1048576,
        },
        {
          id: "deepseek-ai/DeepSeek-V4-Pro-0813",
          name: "DeepSeek V4 Pro 0813",
          context_length: 1048576,
        },
        { id: "moonshotai/Kimi-K3", name: "Kimi K3", context_length: 1048576 },
        { id: "zai-org/GLM-5.3", name: "GLM-5.3", context_length: 1048576 },
        {
          id: "zai-org/GLM-5.3-Flash",
          name: "GLM-5.3 Flash",
          context_length: 1048576,
        },
      ],
    },
    suggestedDefaults: {
      model: {
        default: "deepseek-ai/DeepSeek-V4.1-Flash-1017",
        provider: "deepinfra",
      },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES["vercel-ai-gateway"],
    settingsConfig: {
      // 与 Vercel CLI 写的自定义供应商同名（vercel.com/docs/ai-gateway/
      // coding-agents/hermes，2026-10-14 核对）；Hermes 内置的 ai-gateway 默认
      // 走裸 /v1，文档推荐改成 /coding-agent/v1。窗口取 GET
      // https://ai-gateway.vercel.sh/v1/models 同日公开元数据
      name: "vercel-ai-gateway",
      base_url: "https://ai-gateway.vercel.sh/coding-agent/v1",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        {
          id: "anthropic/claude-opus-5.5",
          name: "Claude Opus 5.5",
          context_length: 1000000,
        },
        {
          id: "anthropic/claude-sonnet-5.5",
          name: "Claude Sonnet 5.5",
          context_length: 1000000,
        },
        {
          id: "anthropic/claude-haiku-5.5",
          name: "Claude Haiku 5.5",
          context_length: 1000000,
        },
        {
          id: "anthropic/claude-fable-5.1",
          name: "Claude Fable 5.1",
          context_length: 1000000,
        },
        {
          id: "openai/gpt-6-astra",
          name: "GPT-6 Astra",
          context_length: 1050000,
        },
        {
          id: "openai/gpt-6.1-sol",
          name: "GPT-6.1 Sol",
          context_length: 1050000,
        },
        {
          id: "openai/gpt-6-luna",
          name: "GPT-6 Luna",
          context_length: 1050000,
        },
        {
          id: "google/gemini-3.8-flash",
          name: "Gemini 3.8 Flash",
          context_length: 1000000,
        },
        {
          id: "google/gemini-4-argon",
          name: "Gemini 4 Argon",
          context_length: 1000000,
        },
      ],
    },
    suggestedDefaults: {
      model: {
        default: "anthropic/claude-sonnet-5.5",
        provider: "vercel-ai-gateway",
      },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES["nebius-token-factory"],
    settingsConfig: {
      name: "nebius",
      // OpenAI Chat 端点 base 为 /v1（docs.tokenfactory.nebius.com/switch.md，
      // 2026-10-14 核对），Bearer 鉴权。窗口取官方公开目录
      // tokenfactory.nebius.com/api/public/models_info 同日快照
      base_url: "https://api.tokenfactory.nebius.com/v1",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        { id: "zai-org/GLM-5.3", name: "GLM-5.3", context_length: 1024000 },
        {
          id: "zai-org/GLM-5.3-Flash",
          name: "GLM-5.3-Flash",
          context_length: 1024000,
        },
        { id: "moonshotai/Kimi-K3", name: "Kimi K3", context_length: 1024000 },
        {
          id: "moonshotai/Kimi-K2.7-Code",
          name: "Kimi K2.7 Code",
          context_length: 262144,
        },
        {
          id: "deepseek-ai/DeepSeek-V4-Pro-0813",
          name: "DeepSeek V4 Pro 0813",
          context_length: 979000,
        },
        {
          id: "deepseek-ai/DeepSeek-V4.1-Flash",
          name: "DeepSeek V4.1 Flash",
          context_length: 1048000,
        },
        {
          id: "MiniMaxAI/MiniMax-M3",
          name: "MiniMax M3",
          context_length: 1049000,
        },
        {
          id: "MiniMaxAI/MiniMax-M3.1-Flash",
          name: "MiniMax M3.1 Flash",
          context_length: 1049000,
        },
      ],
    },
    suggestedDefaults: {
      model: { default: "zai-org/GLM-5.3", provider: "nebius" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES.groq,
    settingsConfig: {
      name: "groq",
      // OpenAI Chat 端点 base 为 /openai/v1（console.groq.com/docs/openai，
      // 2026-10-14 核对），Bearer 鉴权。只收自助可用的 Production 模型 gpt-oss
      // 两款，窗口取 console.groq.com/docs/models 同日值
      base_url: "https://api.groq.com/openai/v1",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        {
          id: "openai/gpt-oss-120b",
          name: "GPT-OSS 120B",
          context_length: 131072,
        },
        {
          id: "openai/gpt-oss-20b",
          name: "GPT-OSS 20B",
          context_length: 131072,
        },
      ],
    },
    suggestedDefaults: {
      model: { default: "openai/gpt-oss-120b", provider: "groq" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES.sambanova,
    settingsConfig: {
      name: "sambanova",
      // OpenAI Chat 端点 base 为 /v1（docs.sambanova.ai/docs/en/integrations/
      // opencode，2026-10-14 核对），Bearer 鉴权，窗口取公开 /v1/models 同日值。
      // 不选 anthropic_messages（OpenAPI 写明 v1 的 thinking enabled/adaptive
      // 返回 400），也不选 codex_responses（Responses 只收 function 工具）
      base_url: "https://api.sambanova.ai/v1",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        { id: "MiniMax-M3", name: "MiniMax M3", context_length: 1048576 },
        { id: "gpt-oss-120b", name: "gpt-oss-120b", context_length: 131072 },
      ],
    },
    suggestedDefaults: {
      model: { default: "MiniMax-M3", provider: "sambanova" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES.digitalocean,
    settingsConfig: {
      name: "digitalocean",
      // OpenAI Chat 端点 base 为 /v1（docs.digitalocean.com/products/inference/
      // how-to/use-with-coding-agents，2026-10-14 核对），Bearer 鉴权；窗口取
      // 平台模型表（docs.digitalocean.com/products/inference/details/models
      // 同日）。四款都支持 Chat 与工具调用；Sonnet 5.5 须 Tier 3+ 账号，默认
      // 用所有等级可用的 GLM-5.3
      base_url: "https://inference.do-ai.run/v1",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        { id: "glm-5.3", name: "GLM-5.3", context_length: 1048576 },
        { id: "kimi-k3", name: "Kimi K3", context_length: 1048576 },
        {
          id: "deepseek-v4-pro-0813",
          name: "DeepSeek V4 Pro 0813",
          context_length: 1048576,
        },
        {
          id: "anthropic-claude-sonnet-5.5",
          name: "Claude Sonnet 5.5",
          context_length: 1000000,
        },
      ],
    },
    suggestedDefaults: {
      model: { default: "glm-5.3", provider: "digitalocean" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES["hugging-face"],
    settingsConfig: {
      // Hermes 内置 provider huggingface（别名 hf，hermes-agent.nousresearch.com/
      // docs/integrations/providers，2026-10-14 核对），与 custom_providers 同名
      // 时谁优先文档没写，照 ollama_cloud 先例换个名字避开
      name: "huggingface-router",
      // OpenAI Chat 端点 base 为 https://router.huggingface.co/v1
      //（huggingface.co/docs/inference-providers/integrations/hermes-agent 同日），
      // Bearer 鉴权；Hermes 文档说可用 :provider_name 后缀强制上游，这里钉
      // :baseten（同 Claude Code 预设），窗口取公开 /v1/models 对 Baseten 的
      // 同日声明
      base_url: "https://router.huggingface.co/v1",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        {
          id: "zai-org/GLM-5.3:baseten",
          name: "GLM-5.3 (Baseten)",
          context_length: 1048576,
        },
        {
          id: "zai-org/GLM-5.3-Flash:baseten",
          name: "GLM-5.3 Flash (Baseten)",
          context_length: 1048576,
        },
        {
          id: "moonshotai/Kimi-K3:baseten",
          name: "Kimi K3 (Baseten)",
          context_length: 1048576,
        },
        {
          id: "deepseek-ai/DeepSeek-V4-Pro-0813:baseten",
          name: "DeepSeek V4 Pro 0813 (Baseten)",
          context_length: 1048576,
        },
        {
          id: "deepseek-ai/DeepSeek-V4.1-Flash-1017:baseten",
          name: "DeepSeek V4.1 Flash 1017 (Baseten)",
          context_length: 1048576,
        },
      ],
    },
    suggestedDefaults: {
      model: {
        default: "zai-org/GLM-5.3:baseten",
        provider: "huggingface-router",
      },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES["fireworks-ai"],
    settingsConfig: {
      // Hermes 内置 provider fireworks（别名 fireworks-ai / fw，hermes-agent.
      // nousresearch.com/docs/integrations/providers，2026-10-14 核对），与
      // custom_providers 同名时谁优先文档没写，照 huggingface-router 先例换个
      // 名字避开
      name: "fireworks-inference",
      // OpenAI Chat 端点 base 为 https://api.fireworks.ai/inference/v1
      //（docs.fireworks.ai/nexus/harnesses 同日），Bearer 鉴权，模型写全名；
      // 窗口取各模型页与定价页（同日），MiniMax M3 只开 512K
      base_url: "https://api.fireworks.ai/inference/v1",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        {
          id: "accounts/fireworks/models/glm-5p3",
          name: "GLM 5.3",
          context_length: 1048576,
        },
        {
          id: "accounts/fireworks/models/kimi-k3",
          name: "Kimi K3",
          context_length: 1048576,
        },
        {
          id: "accounts/fireworks/models/deepseek-v4p1-flash",
          name: "DeepSeek V4.1 Flash",
          context_length: 1000000,
        },
        {
          id: "accounts/fireworks/models/glm-5p3-flash",
          name: "GLM 5.3 Flash",
          context_length: 1048576,
        },
        {
          id: "accounts/fireworks/models/minimax-m3",
          name: "MiniMax M3",
          context_length: 512000,
        },
      ],
    },
    suggestedDefaults: {
      model: {
        default: "accounts/fireworks/models/glm-5p3",
        provider: "fireworks-inference",
      },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES["mistral-ai"],
    settingsConfig: {
      // Hermes 内置的 Mistral 只有语音与转写，没有 LLM provider，名字不冲突
      name: "mistral",
      // OpenAI Chat 端点 base 为 https://api.mistral.ai/v1（docs.mistral.ai/
      // openapi.yaml，2026-10-14 核对），Bearer 鉴权。模型用官方模型表
      //（docs.mistral.ai/models.md 同日）的固定版本 ID，窗口官方写 256k
      base_url: "https://api.mistral.ai/v1",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        {
          id: "mistral-medium-3-5",
          name: "Mistral Medium 3.5",
          context_length: 256000,
        },
        {
          id: "mistral-small-2603",
          name: "Mistral Small 4",
          context_length: 256000,
        },
      ],
    },
    suggestedDefaults: {
      model: { default: "mistral-medium-3-5", provider: "mistral" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES["cloudflare-ai-gateway"],
    settingsConfig: {
      // Hermes 没有内置的 Cloudflare provider，名字不冲突
      name: "cloudflare",
      // REST 统一 API 的 OpenAI Chat 端点（developers.cloudflare.com/ai-gateway/
      // usage/rest-api，2026-10-14 核对），标准 Bearer 鉴权，token 须带 Workers
      // AI Read；账户 ID 用字面占位符，忘了改会 401。模型与窗口同 OpenClaw 预设
      //（developers.cloudflare.com/ai/models 同日），Claude 只有 Messages 格式不放
      base_url:
        "https://api.cloudflare.com/client/v4/accounts/YOUR_ACCOUNT_ID/ai/v1",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        {
          id: "openai/gpt-6.1-sol",
          name: "GPT-6.1 Sol",
          context_length: 1050000,
        },
        {
          id: "openai/gpt-6-luna",
          name: "GPT-6 Luna",
          context_length: 1050000,
        },
        {
          id: "moonshotai/kimi-k3",
          name: "Kimi K3",
          context_length: 1048576,
        },
      ],
    },
    suggestedDefaults: {
      model: { default: "openai/gpt-6.1-sol", provider: "cloudflare" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES.pipellm,
    settingsConfig: {
      name: "pipellm",
      base_url: "https://cc-api.pipellm.ai",
      api_key: "",
      api_mode: "anthropic_messages",
      models: [
        {
          id: "claude-opus-5-5",
          name: "Claude Opus 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-sonnet-5-5",
          name: "Claude Sonnet 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-haiku-5-5",
          name: "Claude Haiku 5.5",
          context_length: 1000000,
        },
      ],
    },
    suggestedDefaults: {
      model: { default: "claude-opus-5-5", provider: "pipellm" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES["xiaomi-mimo"],
    settingsConfig: {
      name: "xiaomi_mimo",
      base_url: "https://api.xiaomimimo.com/v1",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        { id: "mimo-v2.6-pro", name: "MiMo V2.6 Pro", context_length: 1048576 },
        {
          id: "mimo-v2.6-flash",
          name: "MiMo V2.6 Flash",
          context_length: 1048576,
        },
        {
          id: "mimo-v2.6-pro-ultraspeed",
          name: "MiMo V2.6 Pro UltraSpeed",
          context_length: 1048576,
        },
      ],
    },
    suggestedDefaults: {
      model: { default: "mimo-v2.6-pro", provider: "xiaomi_mimo" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES["xiaomi-mimo-token-plan-china"],
    settingsConfig: {
      name: "xiaomi_mimo_token_plan",
      base_url: "https://token-plan-cn.xiaomimimo.com/v1",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        { id: "mimo-v2.6-pro", name: "MiMo V2.6 Pro", context_length: 1048576 },
        {
          id: "mimo-v2.6-flash",
          name: "MiMo V2.6 Flash",
          context_length: 1048576,
        },
      ],
    },
    suggestedDefaults: {
      model: { default: "mimo-v2.6-pro", provider: "xiaomi_mimo_token_plan" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES["jiekou-ai"],
    settingsConfig: {
      name: "jiekou",
      base_url: "https://api.jiekou.ai/openai/v1",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        {
          id: "claude-fable-5-1",
          name: "Claude Fable 5.1",
          context_length: 1000000,
        },
        {
          id: "claude-opus-5-5",
          name: "Claude Opus 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-sonnet-5-5",
          name: "Claude Sonnet 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-haiku-5-5",
          name: "Claude Haiku 5.5",
          context_length: 1000000,
        },
      ],
    },
    suggestedDefaults: {
      model: { default: "claude-fable-5-1", provider: "jiekou" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES.aicodewith,
    settingsConfig: {
      name: "aicodewith",
      base_url: "https://api.aicodewith.ai/v1",
      api_key: "",
      api_mode: "codex_responses",
      models: [
        { id: "gpt-6.1-sol", name: "GPT-6.1 Sol" },
        { id: "gpt-6-sol", name: "GPT-6 Sol", context_length: 272000 },
        { id: "gpt-6-astra", name: "GPT-6 Astra", context_length: 272000 },
      ],
    },
    suggestedDefaults: {
      model: { default: "gpt-6.1-sol", provider: "aicodewith" },
    },
  },
  {
    name: "SoruxGPT",
    websiteUrl: "https://ai.soruxgpt.com",
    apiKeyUrl: "https://ai.soruxgpt.com/console/token",
    settingsConfig: {
      name: "soruxgpt",
      base_url: "https://ai.soruxgpt.com",
      api_key: "",
      api_mode: "anthropic_messages",
      models: [
        {
          id: "claude-opus-5-5",
          name: "Claude Opus 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-sonnet-5-5",
          name: "Claude Sonnet 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-haiku-5-5",
          name: "Claude Haiku 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-fable-5-1",
          name: "Claude Fable 5.1",
          context_length: 1000000,
        },
      ],
    },
    category: "aggregator",
    suggestedDefaults: {
      model: { default: "claude-opus-5-5", provider: "soruxgpt" },
    },
    icon: "soruxgpt",
  },
  {
    ...PRESET_BRAND_IDENTITIES.ikuncode,
    settingsConfig: {
      name: "ikuncode",
      base_url: "https://api.ikuncode.ai",
      api_key: "",
      api_mode: "anthropic_messages",
      models: [
        {
          id: "claude-opus-5-5",
          name: "Claude Opus 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-sonnet-5-5",
          name: "Claude Sonnet 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-haiku-5-5",
          name: "Claude Haiku 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-fable-5-1",
          name: "Claude Fable 5.1",
          context_length: 1000000,
        },
      ],
    },
    suggestedDefaults: {
      model: { default: "claude-opus-5-5", provider: "ikuncode" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES.runtoken,
    settingsConfig: {
      name: "runtoken",
      base_url: "https://www.runtoken.ai",
      api_key: "",
      api_mode: "anthropic_messages",
      models: [
        {
          id: "claude-opus-5-5",
          name: "Claude Opus 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-sonnet-5-5",
          name: "Claude Sonnet 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-haiku-5-5",
          name: "Claude Haiku 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-fable-5-1",
          name: "Claude Fable 5.1",
          context_length: 1000000,
        },
      ],
    },
    suggestedDefaults: {
      model: { default: "claude-opus-5-5", provider: "runtoken" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES.botcf,
    settingsConfig: {
      name: "botcf",
      base_url: "https://botcf.com",
      api_key: "",
      api_mode: "anthropic_messages",
      models: [
        {
          id: "claude-opus-5-5",
          name: "Claude Opus 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-sonnet-5-5",
          name: "Claude Sonnet 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-haiku-5-5",
          name: "Claude Haiku 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-fable-5-1",
          name: "Claude Fable 5.1",
          context_length: 1000000,
        },
      ],
    },
    suggestedDefaults: {
      model: { default: "claude-opus-5-5", provider: "botcf" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES.yylx,
    settingsConfig: {
      name: "yylx",
      base_url: "https://app.yylx.io",
      api_key: "",
      api_mode: "anthropic_messages",
      models: [
        {
          id: "claude-opus-5-5",
          name: "Claude Opus 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-sonnet-5-5",
          name: "Claude Sonnet 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-haiku-5-5",
          name: "Claude Haiku 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-fable-5-1",
          name: "Claude Fable 5.1",
          context_length: 1000000,
        },
      ],
    },
    suggestedDefaults: {
      model: { default: "claude-opus-5-5", provider: "yylx" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES.tokenflux,
    settingsConfig: {
      name: "tokenflux",
      base_url: "https://tokenflux.dev",
      api_key: "",
      api_mode: "anthropic_messages",
      models: [
        {
          id: "claude-opus-5-5",
          name: "Claude Opus 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-sonnet-5-5",
          name: "Claude Sonnet 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-haiku-5-5",
          name: "Claude Haiku 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-fable-5-1",
          name: "Claude Fable 5.1",
          context_length: 1000000,
        },
      ],
    },
    suggestedDefaults: {
      model: { default: "claude-opus-5-5", provider: "tokenflux" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES.nekocode,
    settingsConfig: {
      name: "nekocode",
      base_url: "https://nekoapi.ai",
      api_key: "",
      api_mode: "anthropic_messages",
      extra_headers: {
        "User-Agent": "claude-cli/2.1.161 (external, cli)",
      },
      models: [
        {
          id: "claude-opus-5-5",
          name: "Claude Opus 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-sonnet-5-5",
          name: "Claude Sonnet 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-haiku-5-5",
          name: "Claude Haiku 5.5",
          context_length: 1000000,
        },
      ],
    },
    suggestedDefaults: {
      model: { default: "claude-opus-5-5", provider: "nekocode" },
    },
  },
  {
    ...PRESET_BRAND_IDENTITIES.aicoming,
    settingsConfig: {
      name: "aicoming",
      base_url: "https://api.aicoming.top",
      api_key: "",
      api_mode: "anthropic_messages",
      models: [
        {
          id: "claude-opus-5-5",
          name: "Claude Opus 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-sonnet-5-5",
          name: "Claude Sonnet 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-haiku-5-5",
          name: "Claude Haiku 5.5",
          context_length: 1000000,
        },
        {
          id: "claude-fable-5-1",
          name: "Claude Fable 5.1",
          context_length: 1000000,
        },
      ],
    },
    suggestedDefaults: {
      model: { default: "claude-opus-5-5", provider: "aicoming" },
    },
  },
];
