import { lazy, Suspense } from "react";
import { Icon } from "../../lib/icons";
import { useAppStore } from "../../store";

const SettingsDialog = lazy(() =>
  import("../SettingsDialog").then((module) => ({ default: module.SettingsDialog })),
);

export function SidebarFooter() {
  const settingsTab = useAppStore((s) => s.settingsTab);
  const openSettings = useAppStore((s) => s.openSettings);
  const closeSettings = useAppStore((s) => s.closeSettings);

  return (
    <div className="relative mt-auto border-t border-line/40 px-3 py-2">
      {settingsTab ? (
        <Suspense fallback={null}>
          <SettingsDialog
            tab={settingsTab}
            onClose={closeSettings}
            onTabChange={openSettings}
          />
        </Suspense>
      ) : null}

      <button
        type="button"
        className="group flex w-full items-center gap-2.5 rounded-lg px-2.5 py-2 text-left transition-colors hover:bg-line/40 focus-visible:outline-2 focus-visible:outline-accent"
        aria-label="Settings"
        title="Settings"
        onClick={() => openSettings("models")}
      >
        <Icon.Settings size={15} className="shrink-0 text-faint group-hover:text-ink transition-colors" />
        <span className="text-xs font-medium text-ink">Settings</span>
      </button>
    </div>
  );
}
