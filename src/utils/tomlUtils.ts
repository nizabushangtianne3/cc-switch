import { parse as parseToml } from "smol-toml";
import { normalizeTomlText } from "@/utils/textNormalization";

/**
 * 验证 TOML 格式并转换为 JSON 对象
 * @param text TOML 文本
 * @returns 错误信息（空字符串表示成功）
 */
export const validateToml = (text: string): string => {
  if (!text.trim()) return "";
  try {
    const normalized = normalizeTomlText(text);
    const parsed = parseToml(normalized);
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
      return "mustBeObject";
    }
    return "";
  } catch (e: any) {
    // 返回底层错误信息，由上层进行 i18n 包装
    return e?.message || "parseError";
  }
};
