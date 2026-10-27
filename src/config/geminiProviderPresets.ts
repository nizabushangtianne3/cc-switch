import { PRESET_BRAND_IDENTITIES } from "./presetBrandIdentities";
import type { ProviderCategory } from "@/types";
import type { PresetFamilyFields } from "./presetFamilies";

/**
 * Gemini 预设供应商的视觉主题配置
 */
export interface GeminiPresetTheme {
  /** 图标类型：'gemini' | 'generic' */
  icon?: "gemini" | "generic";
  /** 背景色（选中状态），支持 hex 颜色 */
  backgroundColor?: string;
  /** 文字色（选中状态），支持 hex 颜色 */
  textColor?: string;
}

export interface GeminiProviderPreset extends PresetFamilyFields {
  name: string;
  nameKey?: string; // i18n key for localized display name
  websiteUrl: string;
  apiKeyUrl?: string;
  settingsConfig: object;
  baseURL?: string;
  model?: string;
  description?: string;
  category?: ProviderCategory;
  isPartner?: boolean;
  primePartner?: boolean; // 旧版的置顶合作伙伴标记；v7 起界面不再读取，新预设不写
  partnerPromotionKey?: string;
  endpointCandidates?: string[];
  theme?: GeminiPresetTheme;
  // 图标配置
  icon?: string; // 图标名称
  iconColor?: string; // 图标颜色
}

export const geminiProviderPresets: GeminiProviderPreset[] = [
  {
    name: "Google Official",
    websiteUrl: "https://ai.google.dev/",
    apiKeyUrl: "https://aistudio.google.com/apikey",
    settingsConfig: {
      env: {},
    },
    description: "Google 官方 Gemini API (OAuth)",
    category: "official",
    partnerPromotionKey: "google-official",
    theme: {
      icon: "gemini",
      backgroundColor: "#4285F4",
      textColor: "#FFFFFF",
    },
    icon: "gemini",
    iconColor: "#4285F4",
  },
  // ===== 赞助商预设：文件顺序与 README 赞助商表对齐（仅维护约定；应用内一律按显示名排序，不置顶）=====
  {
    ...PRESET_BRAND_IDENTITIES.packycode,
    settingsConfig: {
      env: {
        GOOGLE_GEMINI_BASE_URL: "https://www.packyapi.ai",
        GEMINI_MODEL: "gemini-3.8-flash",
      },
    },
    baseURL: "https://www.packyapi.ai",
    model: "gemini-3.8-flash",
    description: "PackyCode",
    endpointCandidates: [
      "https://www.packyapi.ai",
      "https://cf.api.fan",
      "https://slb-v1.api.fan",
      "https://www.packyapi.com",
    ],
  },
  {
    ...PRESET_BRAND_IDENTITIES.apinebula,
    settingsConfig: {
      env: {
        GOOGLE_GEMINI_BASE_URL: "https://apinebula.ai",
        GEMINI_API_KEY: "",
        GEMINI_MODEL: "gemini-3.8-flash",
      },
    },
    baseURL: "https://apinebula.ai",
    model: "gemini-3.8-flash",
    description: "APINebula",
    endpointCandidates: ["https://apinebula.ai"],
  },
  {
    name: "AICodeMirror",
    websiteUrl: "https://www.aicodemirror.ai",
    apiKeyUrl: "https://www.aicodemirror.ai/register?invitecode=9915W3",
    settingsConfig: {
      env: {
        GOOGLE_GEMINI_BASE_URL: "https://api.aicodemirror.ai/api/gemini",
        GEMINI_MODEL: "gemini-3.8-flash",
      },
    },
    baseURL: "https://api.aicodemirror.ai/api/gemini",
    model: "gemini-3.8-flash",
    description: "AICodeMirror",
    category: "third_party",
    isPartner: true,
    partnerPromotionKey: "aicodemirror",
    endpointCandidates: ["https://api.aicodemirror.ai/api/gemini"],
    icon: "aicodemirror",
    iconColor: "#000000",
  },
  {
    ...PRESET_BRAND_IDENTITIES.openmodel,
    settingsConfig: {
      env: {
        GOOGLE_GEMINI_BASE_URL: "https://api.openmodel.ai",
        GEMINI_MODEL: "gemini-4-argon",
      },
    },
    baseURL: "https://api.openmodel.ai",
    model: "gemini-4-argon",
    description: "OpenModel",
  },
  {
    ...PRESET_BRAND_IDENTITIES.ditai,
    // Gemini 原生 /v1beta 在站点根；网关也认 x-goog-api-key（2026-10-21 用假 Key
    // 探测为 invalid_api_key 而非 missing_api_key）
    settingsConfig: {
      env: {
        GOOGLE_GEMINI_BASE_URL: "https://api.dit.ai",
        GEMINI_API_KEY: "",
        GEMINI_MODEL: "gemini-3.8-flash",
      },
    },
    baseURL: "https://api.dit.ai",
    model: "gemini-3.8-flash",
    description: "DIT.ai",
  },
  {
    ...PRESET_BRAND_IDENTITIES["v-api"],
    // Gemini 原生 /v1beta（含 streamGenerateContent）在站点根，令牌选 gemini 或
    // gemini_cli 分组（gpt.ge/models/gemini-3.8-flash，2026-10-23）
    settingsConfig: {
      env: {
        GOOGLE_GEMINI_BASE_URL: "https://api.gpt.ge",
        GEMINI_MODEL: "gemini-3.8-flash",
      },
    },
    baseURL: "https://api.gpt.ge",
    model: "gemini-3.8-flash",
    description: "V-API",
    // 备用线路取自 gpt.ge/api/status 的 api_list（2026-10-23）
    endpointCandidates: [
      "https://api.gpt.ge",
      "https://api.vveai.com",
      "https://run.v3.cm",
      "https://api1.v3.cm",
      "https://api2.v3.cm",
      "https://us.gpt.ge",
    ],
  },
  {
    ...PRESET_BRAND_IDENTITIES.shengsuanyun,
    settingsConfig: {
      env: {
        GOOGLE_GEMINI_BASE_URL: "https://router.shengsuanyun.com/api",
        GEMINI_MODEL: "google/gemini-3.8-flash",
      },
    },
    baseURL: "https://router.shengsuanyun.com/api",
    model: "google/gemini-3.8-flash",
    description: "Shengsuanyun",
  },
  {
    ...PRESET_BRAND_IDENTITIES.aigocode,
    settingsConfig: {
      env: {
        GOOGLE_GEMINI_BASE_URL: "https://api.aigocode.dev",
        GEMINI_MODEL: "gemini-3.8-flash",
      },
    },
    baseURL: "https://api.aigocode.dev",
    model: "gemini-3.8-flash",
    description: "AIGoCode",
    endpointCandidates: [
      "https://api.aigocode.dev",
      "https://api.aigocode.app",
    ],
  },
  {
    ...PRESET_BRAND_IDENTITIES.qiniu,
    settingsConfig: {
      env: {
        GOOGLE_GEMINI_BASE_URL: "https://api.qnaigc.com/bypass/vertex",
        GEMINI_MODEL: "gemini-3.8-flash",
      },
    },
    baseURL: "https://api.qnaigc.com/bypass/vertex",
    model: "gemini-3.8-flash",
    description: "Qiniu",
    endpointCandidates: [
      "https://api.qnaigc.com/bypass/vertex",
      "https://api.modelink.ai/bypass/vertex",
    ],
  },
  {
    name: "AICoding",
    websiteUrl: "https://aicoding.inc",
    apiKeyUrl: "https://aicoding.inc/i/CCSWITCH",
    settingsConfig: {
      env: {
        GOOGLE_GEMINI_BASE_URL: "https://api.aicoding.inc",
        GEMINI_MODEL: "gemini-3.8-flash",
      },
    },
    baseURL: "https://api.aicoding.inc",
    model: "gemini-3.8-flash",
    description: "AICoding",
    category: "third_party",
    isPartner: true,
    partnerPromotionKey: "aicoding",
    endpointCandidates: ["https://api.aicoding.inc"],
    icon: "aicoding",
    iconColor: "#000000",
  },
  {
    ...PRESET_BRAND_IDENTITIES.subrouter,
    settingsConfig: {
      env: {
        GOOGLE_GEMINI_BASE_URL: "https://subrouter.ai/v1beta",
        GEMINI_MODEL: "gemini-3.8-flash",
      },
    },
    baseURL: "https://subrouter.ai/v1beta",
    model: "gemini-3.8-flash",
    description: "SubRouter",
    endpointCandidates: ["https://subrouter.ai/v1beta"],
  },
  {
    ...PRESET_BRAND_IDENTITIES["88api"],
    settingsConfig: {
      env: {
        GOOGLE_GEMINI_BASE_URL: "https://api.88api.ai",
        GEMINI_API_KEY: "",
        GEMINI_MODEL: "gemini-3.8-flash",
      },
    },
    baseURL: "https://api.88api.ai",
    model: "gemini-3.8-flash",
    description: "88API",
    endpointCandidates: ["https://api.88api.ai", "https://88api.ai"],
  },
  {
    ...PRESET_BRAND_IDENTITIES.apikeyfun,
    settingsConfig: {
      env: {
        GOOGLE_GEMINI_BASE_URL: "https://api.apikey.fan",
        GEMINI_API_KEY: "",
        GEMINI_MODEL: "gemini-3.8-flash",
      },
    },
    baseURL: "https://api.apikey.fan",
    model: "gemini-3.8-flash",
    description: "APIKEY.FUN",
    endpointCandidates: [
      "https://api.apikey.fan",
      "https://api.apikey.fun",
      "https://slb.apikey.fun",
    ],
  },
  {
    ...PRESET_BRAND_IDENTITIES["9527code"],
    settingsConfig: {
      env: {
        GOOGLE_GEMINI_BASE_URL: "https://9527.codes",
        GEMINI_API_KEY: "",
        GEMINI_MODEL: "gemini-3.8-flash",
      },
    },
    baseURL: "https://9527.codes",
    model: "gemini-3.8-flash",
    description: "9527CODE",
    endpointCandidates: [
      "https://9527.codes",
      "https://api.9527.codes",
      "https://cdn.9527.codes",
    ],
  },
  {
    ...PRESET_BRAND_IDENTITIES.code0,
    settingsConfig: {
      env: {
        GOOGLE_GEMINI_BASE_URL: "https://code0.ai",
        GEMINI_MODEL: "gemini-3.8-flash",
      },
    },
    baseURL: "https://code0.ai",
    model: "gemini-3.8-flash",
    description: "Code0",
  },
  {
    ...PRESET_BRAND_IDENTITIES.a6api,
    settingsConfig: {
      env: {
        GOOGLE_GEMINI_BASE_URL: "https://api.a6api.com",
        GEMINI_MODEL: "gemini-3.8-flash",
      },
    },
    baseURL: "https://api.a6api.com",
    model: "gemini-3.8-flash",
    description: "A6API",
  },
  {
    ...PRESET_BRAND_IDENTITIES.sssaicode,
    settingsConfig: {
      env: {
        GOOGLE_GEMINI_BASE_URL: "https://node-hk.sssaicodeapi.com/api",
        GEMINI_MODEL: "gemini-3.8-flash",
      },
    },
    baseURL: "https://node-hk.sssaicodeapi.com/api",
    model: "gemini-3.8-flash",
    description: "SSSAiCode",
    endpointCandidates: [
      "https://node-hk.sssaicodeapi.com/api",
      "https://node-hk.sssaiapi.com/api",
      "https://node-cf.sssaicodeapi.com/api",
    ],
  },
  {
    ...PRESET_BRAND_IDENTITIES.soleapi,
    settingsConfig: {
      env: {
        GOOGLE_GEMINI_BASE_URL: "https://soleapi.com",
        GEMINI_API_KEY: "",
        GEMINI_MODEL: "gemini-3.8-flash",
      },
    },
    baseURL: "https://soleapi.com",
    model: "gemini-3.8-flash",
    description: "SoleAPI",
    endpointCandidates: ["https://soleapi.com"],
  },
  {
    ...PRESET_BRAND_IDENTITIES.cubence,
    settingsConfig: {
      env: {
        GOOGLE_GEMINI_BASE_URL: "https://api.cubence.com",
        GEMINI_MODEL: "gemini-3.8-flash",
      },
    },
    baseURL: "https://api.cubence.com",
    model: "gemini-3.8-flash",
    description: "Cubence",
    endpointCandidates: [
      "https://api.cubence.com/v1",
      "https://api-cf.cubence.com/v1",
      "https://api-dmit.cubence.com/v1",
      "https://api-bwg.cubence.com/v1",
    ],
  },
  {
    name: "CrazyRouter",
    websiteUrl: "https://www.crazyrouter.com",
    apiKeyUrl: "https://www.crazyrouter.com/register?aff=OZcm&ref=cc-switch",
    settingsConfig: {
      env: {
        GOOGLE_GEMINI_BASE_URL: "https://cn.crazyrouter.com",
        GEMINI_MODEL: "gemini-3.8-flash",
      },
    },
    baseURL: "https://cn.crazyrouter.com",
    model: "gemini-3.8-flash",
    description: "CrazyRouter",
    category: "third_party",
    isPartner: true,
    partnerPromotionKey: "crazyrouter",
    endpointCandidates: ["https://cn.crazyrouter.com"],
    icon: "crazyrouter",
    iconColor: "#000000",
  },
  {
    name: "SudoCode.us",
    websiteUrl: "https://sudocode.us",
    apiKeyUrl: "https://sudocode.us",
    settingsConfig: {
      env: {
        GOOGLE_GEMINI_BASE_URL: "https://sudocode.us",
        GEMINI_API_KEY: "",
        GEMINI_MODEL: "gemini-3.8-flash",
      },
    },
    baseURL: "https://sudocode.us",
    model: "gemini-3.8-flash",
    description: "SudoCode.us",
    category: "third_party",
    isPartner: true,
    endpointCandidates: ["https://sudocode.us", "https://sudocode.run"],
    icon: "sudocode-us",
  },
  {
    ...PRESET_BRAND_IDENTITIES.xycai,
    settingsConfig: {
      env: {
        GOOGLE_GEMINI_BASE_URL: "https://apicdn.xycai.us",
        GEMINI_API_KEY: "",
        GEMINI_MODEL: "gemini-3.8-flash",
      },
    },
    baseURL: "https://apicdn.xycai.us",
    model: "gemini-3.8-flash",
    description: "XycAi",
    endpointCandidates: ["https://apicdn.xycai.us", "https://apicdn.xyc.ai"],
  },
  {
    ...PRESET_BRAND_IDENTITIES.modelflare,
    // 官方 Gemini CLI 文档（docs.modelflare.dev/guides/clients/gemini-cli，
    // 2026-10-15 核对）：base 用站点根。Gemini CLI 走 /v1beta，只有
    // modelflare.dev 提供，origin 子域不提供，故不加备用端点
    settingsConfig: {
      env: {
        GOOGLE_GEMINI_BASE_URL: "https://modelflare.dev",
        GEMINI_API_KEY: "",
        GEMINI_MODEL: "gemini-3.8-flash",
      },
    },
    baseURL: "https://modelflare.dev",
    model: "gemini-3.8-flash",
    description: "Modelflare",
  },
  {
    ...PRESET_BRAND_IDENTITIES.alltokens,
    // Sub2API 网关：Key 绑定单个分组，须用 Gemini 分组的 Key（分组与模型见
    // alltokens.cc/api/v1/model-plaza，2026-10-17 核对）
    settingsConfig: {
      env: {
        GOOGLE_GEMINI_BASE_URL: "https://alltokens.cc",
        GEMINI_API_KEY: "",
        GEMINI_MODEL: "gemini-3.8-flash",
      },
    },
    baseURL: "https://alltokens.cc",
    model: "gemini-3.8-flash",
    description: "AllTokens",
  },
  {
    ...PRESET_BRAND_IDENTITIES["alltokens-si"],
    // 站点关闭了个人注册（/api/status 的 register_enabled=false），只能企业注册
    // Gemini 分组不在默认自动分组里，须用 Gemini 分组的 Key；目录里唯一
    // 规范的 3.8 Flash ID 是 gemini-3.8-flash-high（alltokens.si/api/pricing，
    // 2026-10-19 核对）
    settingsConfig: {
      env: {
        GOOGLE_GEMINI_BASE_URL: "https://alltokens.si",
        GEMINI_API_KEY: "",
        GEMINI_MODEL: "gemini-3.8-flash-high",
      },
    },
    baseURL: "https://alltokens.si",
    model: "gemini-3.8-flash-high",
    description: "AllTokens SI",
  },
  // ===== 非赞助商预设：应用内展示按显示名排序，此处文件顺序不影响展示 =====
  {
    ...PRESET_BRAND_IDENTITIES["tu-zi"],
    settingsConfig: {
      env: {
        GOOGLE_GEMINI_BASE_URL: "https://api.tu-zi.com",
        GEMINI_API_KEY: "",
        GEMINI_MODEL: "gemini-3.8-flash",
      },
    },
    baseURL: "https://api.tu-zi.com",
    model: "gemini-3.8-flash",
    description: "Tu-zi",
    endpointCandidates: [
      "https://api.tu-zi.com",
      "https://api.ourzhishi.top",
      "https://api.sydney-ai.com",
      "https://apicdn.tu-zi.com",
    ],
  },
  {
    ...PRESET_BRAND_IDENTITIES["e-flowcode"],
    settingsConfig: {
      env: {
        GOOGLE_GEMINI_BASE_URL: "https://e-flowcode.cc",
        GEMINI_API_KEY: "",
        GEMINI_MODEL: "gemini-3.8-flash",
      },
      config: {
        general: {
          previewFeatures: true,
          sessionRetention: {
            enabled: true,
            maxAge: "30d",
            warningAcknowledged: true,
          },
        },
        mcpServers: {},
        security: {
          auth: {
            selectedType: "gemini-api-key",
          },
        },
      },
    },
    baseURL: "https://e-flowcode.cc",
    model: "gemini-3.8-flash",
    description: "E-FlowCode",
    endpointCandidates: ["https://e-flowcode.cc"],
  },
  {
    ...PRESET_BRAND_IDENTITIES.cherryin,
    settingsConfig: {
      env: {
        GOOGLE_GEMINI_BASE_URL: "https://open.cherryin.net",
        GEMINI_API_KEY: "",
        GEMINI_MODEL: "google/gemini-3.8-flash",
      },
    },
    baseURL: "https://open.cherryin.net",
    model: "google/gemini-3.8-flash",
    description: "CherryIN",
    endpointCandidates: ["https://open.cherryin.net"],
  },
  {
    name: "OpenRouter",
    websiteUrl: "https://openrouter.ai",
    apiKeyUrl: "https://openrouter.ai/keys",
    settingsConfig: {
      env: {
        GOOGLE_GEMINI_BASE_URL: "https://openrouter.ai/api",
        GEMINI_MODEL: "gemini-3.8-flash",
      },
    },
    baseURL: "https://openrouter.ai/api",
    model: "gemini-3.8-flash",
    description: "OpenRouter",
    category: "aggregator",
    icon: "openrouter",
    iconColor: "#6566F1",
  },
  {
    ...PRESET_BRAND_IDENTITIES.therouter,
    settingsConfig: {
      env: {
        GOOGLE_GEMINI_BASE_URL: "https://api.therouter.ai",
        GEMINI_MODEL: "gemini-3.8-flash",
      },
    },
    baseURL: "https://api.therouter.ai",
    model: "gemini-3.8-flash",
    description: "TheRouter",
    endpointCandidates: ["https://api.therouter.ai"],
  },
  {
    ...PRESET_BRAND_IDENTITIES.aicodewith,
    settingsConfig: {
      env: {
        GOOGLE_GEMINI_BASE_URL: "https://api.aicodewith.ai/gemini_cli",
        GEMINI_MODEL: "gemini-4-argon",
      },
    },
    baseURL: "https://api.aicodewith.ai/gemini_cli",
    model: "gemini-4-argon",
    description: "AICodeWith",
    endpointCandidates: ["https://api.aicodewith.ai/gemini_cli"],
  },
  {
    name: "SoruxGPT",
    websiteUrl: "https://ai.soruxgpt.com",
    apiKeyUrl: "https://ai.soruxgpt.com/console/token",
    settingsConfig: {
      env: {
        GOOGLE_GEMINI_BASE_URL: "https://ai.soruxgpt.com",
        GEMINI_MODEL: "gemini-4-argon",
      },
    },
    baseURL: "https://ai.soruxgpt.com",
    model: "gemini-4-argon",
    description: "SoruxGPT",
    category: "aggregator",
    endpointCandidates: [
      "https://ai.soruxgpt.com",
      "https://api-cf.soruxgpt.com",
      "https://api-host.soruxgpt.com",
      "https://api-node6.soruxgpt.com",
      "https://api-node3.soruxgpt.com",
    ],
    icon: "soruxgpt",
  },
  {
    ...PRESET_BRAND_IDENTITIES.ikuncode,
    settingsConfig: {
      env: {
        GOOGLE_GEMINI_BASE_URL: "https://api.ikuncode.ai",
        GEMINI_MODEL: "gemini-4-argon",
      },
    },
    baseURL: "https://api.ikuncode.ai",
    model: "gemini-4-argon",
    description: "IKunCode",
    endpointCandidates: ["https://api.ikuncode.ai", "https://api.ikuncode.cc"],
  },
  {
    ...PRESET_BRAND_IDENTITIES.runtoken,
    settingsConfig: {
      env: {
        GOOGLE_GEMINI_BASE_URL: "https://www.runtoken.ai",
        GEMINI_MODEL: "gemini-4-argon",
      },
    },
    baseURL: "https://www.runtoken.ai",
    model: "gemini-4-argon",
    description: "RunToken",
    endpointCandidates: ["https://www.runtoken.ai", "https://fast.runtoken.ai"],
  },
  {
    ...PRESET_BRAND_IDENTITIES.yylx,
    settingsConfig: {
      env: {
        GOOGLE_GEMINI_BASE_URL: "https://app.yylx.io",
        GEMINI_MODEL: "gemini-3.8-flash",
      },
    },
    baseURL: "https://app.yylx.io",
    model: "gemini-3.8-flash",
    description: "YYLX",
    endpointCandidates: [
      "https://app.yylx.io",
      "https://global.yylx.io",
      "https://cn.yylx.io",
    ],
  },
  {
    ...PRESET_BRAND_IDENTITIES.tokenflux,
    settingsConfig: {
      env: {
        GOOGLE_GEMINI_BASE_URL: "https://tokenflux.dev",
        GEMINI_MODEL: "gemini-3.8-flash",
      },
    },
    baseURL: "https://tokenflux.dev",
    model: "gemini-3.8-flash",
    description: "TokenFlux",
  },
  {
    ...PRESET_BRAND_IDENTITIES.aicoming,
    settingsConfig: {
      env: {
        GOOGLE_GEMINI_BASE_URL: "https://api.aicoming.top",
        GEMINI_MODEL: "gemini-4-argon",
      },
    },
    baseURL: "https://api.aicoming.top",
    model: "gemini-4-argon",
    description: "AIComing",
  },
];

export function getGeminiPresetByName(
  name: string,
): GeminiProviderPreset | undefined {
  return geminiProviderPresets.find((preset) => preset.name === name);
}

export function getGeminiPresetByUrl(
  url: string,
): GeminiProviderPreset | undefined {
  if (!url) return undefined;
  return geminiProviderPresets.find(
    (preset) =>
      preset.baseURL &&
      url.toLowerCase().includes(preset.baseURL.toLowerCase()),
  );
}
