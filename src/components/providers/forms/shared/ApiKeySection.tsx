import { useTranslation } from "react-i18next";
import ApiKeyInput from "../ApiKeyInput";
import { ApiKeyPoolEditor } from "./ApiKeyPoolEditor";
import type { ApiKeyEntry, ApiKeyStrategy, ProviderCategory } from "@/types";
import { DEFAULT_API_KEY_STRATEGY } from "@/utils/apiKeyPool";

interface ApiKeySectionProps {
  id?: string;
  label?: string;
  value: string;
  onChange: (value: string) => void;
  category?: ProviderCategory;
  shouldShowLink: boolean;
  websiteUrl: string;
  placeholder?: {
    official: string;
    thirdParty: string;
  };
  disabled?: boolean;
  /** 保存时会校验 Key 非空的表单才传；官方 / 云厂商 / 禁用时不标星 */
  required?: boolean;
  isPartner?: boolean;
  partnerPromotionKey?: string;
  /** 支持本地路由的供应商传入 Key 池字段后，自动切换为多 Key UI。 */
  apiKeys?: ApiKeyEntry[];
  apiKeyStrategy?: ApiKeyStrategy;
  onApiKeysChange?: (entries: ApiKeyEntry[]) => void;
  onApiKeyStrategyChange?: (strategy: ApiKeyStrategy) => void;
}

export function ApiKeySection({
  id = "apiKey",
  label = "API Key",
  value,
  onChange,
  category,
  shouldShowLink,
  websiteUrl,
  placeholder,
  disabled,
  required = false,
  partnerPromotionKey,
  apiKeys,
  apiKeyStrategy,
  onApiKeysChange,
  onApiKeyStrategyChange,
}: ApiKeySectionProps) {
  const { t } = useTranslation();

  const defaultPlaceholder = {
    official: t("providerForm.officialNoApiKey", {
      defaultValue: "官方供应商无需 API Key",
    }),
    thirdParty: t("providerForm.apiKeyAutoFill", {
      defaultValue: "输入 API Key，将自动填充到配置",
    }),
  };

  const finalPlaceholder = placeholder || defaultPlaceholder;
  const isDisabled = disabled ?? category === "official";
  const isRequired =
    required &&
    !isDisabled &&
    category !== "official" &&
    category !== "cloud_provider";

  const showLink = shouldShowLink && Boolean(websiteUrl);
  // 推广语跟着「获取 API Key」走，按输入框说明文字的样式写在框下面（v7 不画推广框）
  const promotion =
    showLink && partnerPromotionKey
      ? t(`providerForm.partnerPromotion.${partnerPromotionKey}`, {
          defaultValue: "",
        })
      : "";

  const labelAside = showLink ? (
    <a
      href={websiteUrl}
      target="_blank"
      rel="noopener noreferrer"
      className="text-caption text-blue-400 dark:text-blue-500 hover:text-blue-500 dark:hover:text-blue-400 transition-colors"
    >
      {t("providerForm.getApiKey", { defaultValue: "获取 API Key" })} ↗
    </a>
  ) : null;

  const placeholderText =
    category === "official"
      ? finalPlaceholder.official
      : finalPlaceholder.thirdParty;

  if (apiKeys !== undefined && onApiKeysChange) {
    return (
      <ApiKeyPoolEditor
        id={id}
        label={label}
        entries={apiKeys}
        strategy={apiKeyStrategy ?? DEFAULT_API_KEY_STRATEGY}
        onChange={onApiKeysChange}
        onStrategyChange={onApiKeyStrategyChange ?? (() => {})}
        placeholder={placeholderText}
        disabled={isDisabled}
        required={isRequired}
        labelAside={labelAside}
        hint={promotion || undefined}
        category={category}
      />
    );
  }

  return (
    <ApiKeyInput
      id={id}
      label={label}
      value={value}
      onChange={onChange}
      placeholder={placeholderText}
      disabled={isDisabled}
      required={isRequired}
      labelAside={labelAside}
      hint={promotion || undefined}
    />
  );
}
