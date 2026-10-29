import { Suspense, lazy } from "react";
import { useTranslation } from "react-i18next";
import { FullScreenPanel } from "@/components/common/FullScreenPanel";
import type { AuthCenterTarget } from "@/lib/api";

const AuthCenterPanel = lazy(() =>
  import("@/components/settings/AuthCenterPanel").then((module) => ({
    default: module.AuthCenterPanel,
  })),
);

interface AuthSettingsPanelProps {
  target: AuthCenterTarget | null;
  onClose: () => void;
}

export function AuthSettingsPanel({ target, onClose }: AuthSettingsPanelProps) {
  const { t } = useTranslation();
  const isOpen = target !== null;

  return (
    <FullScreenPanel
      isOpen={isOpen}
      title={t("nav.auth")}
      onClose={onClose}
      motionPreset="slide-from-right"
    >
      {target ? (
        <Suspense
          fallback={
            <div
              aria-busy="true"
              className="py-12 text-center text-body text-fg-2"
            >
              {t("common.loading")}
            </div>
          }
        >
          <AuthCenterPanel authScrollTarget={target} />
        </Suspense>
      ) : null}
    </FullScreenPanel>
  );
}
