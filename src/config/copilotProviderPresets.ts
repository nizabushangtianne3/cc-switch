import { piProviderPresets } from "./piProviderPresets";
import {
  COPILOT_ENV,
  copilotProtocol,
  formatCopilotHeaders,
  type CopilotEnv,
  type CopilotProviderPreset,
} from "./copilotEnv";

// 复用 Pi 已维护的供应商目录（分组、合作信息、地址、模型），换算成 Copilot 的环境变量。
//
// 这个文件只放目录数据：真的要列预设的界面才该引它。环境变量键表、接口格式和 env
// 助手在 ./copilotEnv，那边一条预设目录的值导入都没有——`ProviderCard` 只需要
// 「这张卡是不是 BYOK」，从这儿取会把整份目录拖进首屏包。
export const copilotProviderPresets: CopilotProviderPreset[] =
  piProviderPresets.flatMap((preset) => {
    const { settingsConfig: source, ...identity } = preset;
    const protocol = copilotProtocol(source.api);
    if (!protocol || !source.models.length || !source.baseUrl) return [];
    const env: CopilotEnv = {
      [COPILOT_ENV.baseUrl]: source.baseUrl,
      ...protocol,
      [COPILOT_ENV.apiKey]:
        source.apiKey || (preset.category === "local" ? "local" : ""),
      [COPILOT_ENV.model]: source.models[0].id,
    };
    const headers = formatCopilotHeaders(source.headers);
    if (headers) env[COPILOT_ENV.headers] = headers;
    return [
      {
        ...identity,
        settingsConfig: { env },
        models: source.models.map((model) => model.id),
      },
    ];
  });
