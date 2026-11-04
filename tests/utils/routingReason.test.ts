import { describe, expect, it } from "vitest";
import type { TFunction } from "i18next";
import type { Provider } from "@/types";
import { getRoutingReason } from "@/utils/routingReason";

// 测试里 i18n 用空资源：按 defaultValue 断言
const t = ((key: string, options?: { defaultValue?: string }) =>
  options?.defaultValue ?? key) as unknown as TFunction;

function provider(fields: Record<string, unknown>): Provider {
  return fields as unknown as Provider;
}

describe("getRoutingReason", () => {
  it("says nothing for a provider that can go direct", () => {
    // Claude 原生 Anthropic 格式：不需要本地路由，徽标不该出现。
    expect(
      getRoutingReason(
        "claude",
        provider({ meta: { apiFormat: "anthropic" } }),
        t,
      ),
    ).toBeNull();
  });

  it("leads with the multi-key pool, whatever else the card carries", () => {
    // 多 Key 排在最前：即使格式原生，也要按策略选 Key。
    const card = provider({
      meta: {
        apiFormat: "anthropic",
        apiKeys: [{ key: "k1" }, { key: "k2" }],
      },
    });
    expect(getRoutingReason("claude", card, t)).toBe(
      "配置了多个 API Key，需要本地路由按策略选择 Key",
    );
  });

  it("names the GitHub Copilot card before the managed-OAuth branch", () => {
    // github_copilot 同时是托管 OAuth：专属文案必须赢。
    const card = provider({ meta: { providerType: "github_copilot" } });
    expect(getRoutingReason("claude", card, t)).toBe(
      "使用 GitHub Copilot 作为 Claude 供应商",
    );
  });

  it("names the injected OAuth credential for codex accounts", () => {
    const card = provider({ meta: { providerType: "codex_oauth" } });
    expect(getRoutingReason("codex", card, t)).toBe(
      "使用托管 OAuth 登录（令牌由本地路由注入）",
    );
  });

  it("maps each foreign format onto its own explanation", () => {
    expect(
      getRoutingReason(
        "claude",
        provider({ meta: { apiFormat: "openai_chat" } }),
        t,
      ),
    ).toBe("使用 OpenAI Chat 接口格式");

    expect(
      getRoutingReason(
        "claude",
        provider({ meta: { apiFormat: "openai_responses" } }),
        t,
      ),
    ).toBe("使用 OpenAI Responses 接口格式");

    expect(
      getRoutingReason(
        "codex",
        provider({ meta: { apiFormat: "anthropic" } }),
        t,
      ),
    ).toBe("使用 Anthropic Messages 接口格式");
  });

  it("reads the wire api out of a hand-written Codex config", () => {
    // 面板没写 apiFormat 时从 config.toml 原文里取 wire_api。
    const config = 'model_provider = "custom"\n[model_providers.custom]\nwire_api = "chat"\n';
    expect(
      getRoutingReason("codex", provider({ settingsConfig: { config } }), t),
    ).toBe("使用 OpenAI Chat 接口格式");

    const responses =
      'model_provider = "custom"\n[model_providers.custom]\nwire_api = "responses"\n';
    expect(
      getRoutingReason(
        "codex",
        provider({ settingsConfig: { config: responses } }),
        t,
      ),
    ).toBeNull();
  });

  it("explains the Claude Desktop gateway and the full-url mode", () => {
    expect(
      getRoutingReason(
        "claude-desktop",
        provider({ meta: { claudeDesktopMode: "proxy" } }),
        t,
      ),
    ).toBe("使用模型映射（Claude Desktop 只认 Claude 模型名）");

    const fullUrl = provider({ meta: { isFullUrl: true } });
    expect(getRoutingReason("claude", fullUrl, t)).toBe(
      "开启了完整 URL 连接模式",
    );
  });

  it("falls back to the generic reason when no shape is recognised", () => {
    // Claude 下任何非原生格式都要路由，但认不出具体种类时给通用解释。
    expect(
      getRoutingReason(
        "claude",
        provider({ meta: { apiFormat: "gemini_native" } }),
        t,
      ),
    ).toBe("需要本地路由处理请求");
  });
});
