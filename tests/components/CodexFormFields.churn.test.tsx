import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState, type ComponentProps } from "react";
import { useForm } from "react-hook-form";
import { describe, expect, it, vi } from "vitest";
import { CodexFormFields } from "@/components/providers/forms/CodexFormFields";
import { Form } from "@/components/ui/form";

const modelFetchApiMock = vi.hoisted(() => ({
  fetchModelsForConfig: vi.fn(),
  fetchXaiOauthModels: vi.fn(),
  modelFetchRequestHeaders: vi.fn(),
  showFetchModelsError: vi.fn(),
}));

vi.mock("@/lib/api/model-fetch", () => ({
  fetchModelsForConfig: modelFetchApiMock.fetchModelsForConfig,
  fetchXaiOauthModels: modelFetchApiMock.fetchXaiOauthModels,
  modelFetchRequestHeaders: modelFetchApiMock.modelFetchRequestHeaders,
  showFetchModelsError: modelFetchApiMock.showFetchModelsError,
}));

// 上游起 CodexFormFields 内部使用 useModelMetadataFill（依赖 QueryClientProvider），
// 本文件只测渲染次数，mock 成 no-op。真实 hook 返回 useCallback，mock 也必须返回
// 稳定引用，否则下游 useCallback 每次渲染都重建，memo 契约被 mock 自己打穿。
vi.mock("@/hooks/useModelMetadataFill", () => {
  const fill = () => {};
  return { useModelMetadataFill: () => fill };
});

const sectionSpies = vi.hoisted(() => ({
  apiKeySection: vi.fn(),
  copilotAuthSection: vi.fn(),
  codexOAuthSection: vi.fn(),
  xaiOAuthSection: vi.fn(),
  fetchedModelPicker: vi.fn(),
  endpointSpeedTest: vi.fn(),
}));

// 计数探针：spy 必须包 memo，否则父组件重渲染必然调用它，基准测不到 memo 契约。
// 探针只记录 props 供断言，不渲染真实区块（jsdom 里它们大多也要网络/查询上下文）。
vi.mock("@/components/providers/forms/shared", async (importOriginal) => {
  const actual =
    await importOriginal<
      typeof import("@/components/providers/forms/shared")
    >();
  const { memo } = await vi.importActual<typeof import("react")>("react");
  return {
    ...actual,
    ApiKeySection: memo((props: Record<string, unknown>) => {
      sectionSpies.apiKeySection(props);
      return <div data-testid="api-key-section" />;
    }),
  };
});

vi.mock(
  "@/components/providers/forms/CopilotAuthSection",
  async (importOriginal) => {
    const actual =
      await importOriginal<
        typeof import("@/components/providers/forms/CopilotAuthSection")
      >();
    const { memo } = await vi.importActual<typeof import("react")>("react");
    return {
      ...actual,
      CopilotAuthSection: memo((props: Record<string, unknown>) => {
        sectionSpies.copilotAuthSection(props);
        return <div data-testid="copilot-auth-section" />;
      }),
    };
  },
);

vi.mock(
  "@/components/providers/forms/CodexOAuthSection",
  async (importOriginal) => {
    const actual =
      await importOriginal<
        typeof import("@/components/providers/forms/CodexOAuthSection")
      >();
    const { memo } = await vi.importActual<typeof import("react")>("react");
    return {
      ...actual,
      CodexOAuthSection: memo((props: Record<string, unknown>) => {
        sectionSpies.codexOAuthSection(props);
        return <div data-testid="codex-oauth-section" />;
      }),
    };
  },
);

vi.mock(
  "@/components/providers/forms/XaiOAuthSection",
  async (importOriginal) => {
    const actual =
      await importOriginal<
        typeof import("@/components/providers/forms/XaiOAuthSection")
      >();
    const { memo } = await vi.importActual<typeof import("react")>("react");
    return {
      ...actual,
      XaiOAuthSection: memo((props: Record<string, unknown>) => {
        sectionSpies.xaiOAuthSection(props);
        return <div data-testid="xai-oauth-section" />;
      }),
    };
  },
);

vi.mock(
  "@/components/providers/forms/FetchedModelPicker",
  async (importOriginal) => {
    const actual =
      await importOriginal<
        typeof import("@/components/providers/forms/FetchedModelPicker")
      >();
    const { memo } = await vi.importActual<typeof import("react")>("react");
    return {
      ...actual,
      FetchedModelPicker: memo((props: Record<string, unknown>) => {
        sectionSpies.fetchedModelPicker(props);
        return <div data-testid="fetched-model-picker" />;
      }),
    };
  },
);

vi.mock(
  "@/components/providers/forms/EndpointSpeedTest",
  async (importOriginal) => {
    const actual =
      await importOriginal<
        typeof import("@/components/providers/forms/EndpointSpeedTest")
      >();
    const { memo } = await vi.importActual<typeof import("react")>("react");
    return {
      ...actual,
      default: memo((props: Record<string, unknown>) => {
        sectionSpies.endpointSpeedTest(props);
        return <div data-testid="endpoint-speed-test" />;
      }),
    };
  },
);

const inputSpy = vi.fn();

// 模型目录行的输入：计数探针包 memo，测行的 memo 契约（不包的话父组件重渲染必然
// 调用它）。渲染真 input，onChange 按真实 Input 的调用形状转发。
vi.mock("@/components/ui/input", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/components/ui/input")>();
  const { memo } = await vi.importActual<typeof import("react")>("react");
  return {
    ...actual,
    Input: memo((props: Record<string, unknown>) => {
      inputSpy(props);
      return (
        <input
          id={props.id as string}
          aria-label={props["aria-label"] as string}
          value={props.value as string}
          type={(props.type as string) ?? "text"}
          onChange={(event) => {
            const onChange = props.onChange as
              | ((event: { target: { value: string } }) => void)
              | undefined;
            onChange?.({ target: { value: event.target.value } });
          }}
        />
      );
    }),
  };
});

type CodexFormFieldsProps = ComponentProps<typeof CodexFormFields>;

function baseProps(
  overrides: Partial<CodexFormFieldsProps> = {},
): CodexFormFieldsProps {
  return {
    providerId: "p1",
    codexApiKey: "sk-test",
    onApiKeyChange: vi.fn(),
    category: "third_party",
    shouldShowApiKeyLink: false,
    websiteUrl: "",
    codexBaseUrl: "https://api.example.com",
    onBaseUrlChange: vi.fn(),
    isFullUrl: false,
    onFullUrlChange: vi.fn(),
    isEndpointModalOpen: false,
    onEndpointModalToggle: vi.fn(),
    autoSelect: false,
    onAutoSelectChange: vi.fn(),
    codexModel: "",
    onModelChange: vi.fn(),
    apiFormat: "openai_responses",
    onApiFormatChange: vi.fn(),
    anthropicAuthField: "ANTHROPIC_AUTH_TOKEN",
    onAnthropicAuthFieldChange: vi.fn(),
    impersonateClaudeCode: false,
    onImpersonateClaudeCodeChange: vi.fn(),
    maxOutputTokens: "",
    onMaxOutputTokensChange: vi.fn(),
    promptCacheRouting: "auto",
    onPromptCacheRoutingChange: vi.fn(),
    speedTestEndpoints: [],
    customUserAgent: "",
    onCustomUserAgentChange: vi.fn(),
    localProxyHeadersOverride: "",
    onLocalProxyHeadersOverrideChange: vi.fn(),
    localProxyBodyOverride: "",
    onLocalProxyBodyOverrideChange: vi.fn(),
    shouldShowSpeedTest: true,
    catalogModels: [],
    onCatalogModelsChange: vi.fn(),
    ...overrides,
  };
}

// harness 持有默认模型 state：键入时像真实表单那样逐字符触发父组件重渲染。
// props 每次测试只建一次并当参数传进去：真实父组件（ProviderForm）给的是稳定引用
// 回调；若在组件体内每次渲染新建 vi.fn()，memo 契约会被测试自己打穿。
function Harness({ props }: { props: CodexFormFieldsProps }) {
  const [model, setModel] = useState("");
  const form = useForm();
  return (
    <Form {...form}>
      <CodexFormFields {...props} codexModel={model} onModelChange={setModel} />
    </Form>
  );
}

const modelInput = () =>
  document.querySelector("#codexDefaultModel") as HTMLInputElement;

describe("CodexFormFields render churn", () => {
  it("typing the default model leaves the API key section and model picker untouched", async () => {
    const user = userEvent.setup();
    render(<Harness props={baseProps()} />);

    expect(sectionSpies.apiKeySection).toHaveBeenCalledTimes(1);
    expect(sectionSpies.fetchedModelPicker).toHaveBeenCalledTimes(1);

    await user.type(modelInput(), "gpt-5");

    expect(sectionSpies.apiKeySection).toHaveBeenCalledTimes(1);
    expect(sectionSpies.fetchedModelPicker).toHaveBeenCalledTimes(1);
  });

  it("typing the default model leaves OAuth account sections untouched", async () => {
    const user = userEvent.setup();
    render(<Harness props={baseProps({ isCodexOauthPreset: true })} />);

    expect(sectionSpies.codexOAuthSection).toHaveBeenCalledTimes(1);
    expect(sectionSpies.apiKeySection).not.toHaveBeenCalled();

    await user.type(modelInput(), "gpt-5");

    expect(sectionSpies.codexOAuthSection).toHaveBeenCalledTimes(1);
  });

  it("typing the default model leaves the endpoint speed test untouched", async () => {
    const user = userEvent.setup();
    render(<Harness props={baseProps({ isEndpointModalOpen: true })} />);

    expect(sectionSpies.endpointSpeedTest).toHaveBeenCalledTimes(1);

    await user.type(modelInput(), "gpt-5");

    expect(sectionSpies.endpointSpeedTest).toHaveBeenCalledTimes(1);
  });
});

describe("CodexFormFields catalog row churn", () => {
  it("typing in one catalog row leaves the other rows untouched", async () => {
    const user = userEvent.setup();
    render(
      <Harness
        props={baseProps({
          catalogModels: [
            { model: "m-one", displayName: "One" },
            { model: "m-two", displayName: "Two" },
            { model: "m-three", displayName: "Three" },
          ],
        })}
      />,
    );

    const rowRendersFor = (value: string) =>
      inputSpy.mock.calls.filter(
        ([props]) =>
          props["aria-label"] === "菜单显示名" && props.value === value,
      ).length;

    const twoAtMount = rowRendersFor("Two");
    const threeAtMount = rowRendersFor("Three");
    expect(twoAtMount).toBeGreaterThanOrEqual(1);
    expect(threeAtMount).toBe(twoAtMount);

    await user.type(screen.getAllByLabelText("菜单显示名")[0], "X");

    // 只有被键入那一行自己重渲染（值变了），其余行一次都不多跑
    expect(rowRendersFor("Two")).toBe(twoAtMount);
    expect(rowRendersFor("Three")).toBe(threeAtMount);
  });
});
