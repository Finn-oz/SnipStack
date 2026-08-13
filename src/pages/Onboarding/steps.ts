import { findPreferenceSectionSettings } from "@/pages/Preference/config/preferenceSchema";
import DoneStep from "./components/DoneStep";
import IgnoreAppsStep from "./components/IgnoreAppsStep";
import LegacyImportStep from "./components/LegacyImportStep";
import PermissionsStep from "./components/PermissionsStep";
import ShortcutsStep from "./components/ShortcutsStep";
import WelcomeStep from "./components/WelcomeStep";
import type { OnboardingStepDefinition } from "./types";

export const ONBOARDING_STEPS: OnboardingStepDefinition[] = [
  {
    component: WelcomeStep,
    icon: "i-lucide:sparkles",
    id: "welcome",
  },
  // 权限步骤跟随 schema:当前平台没有任何权限设置项时整步跳过
  // (Windows 在 v0.1.0 隐藏了管理员选项后即属此情形)。
  ...(findPreferenceSectionSettings("permissions").length > 0
    ? [
        {
          component: PermissionsStep,
          icon: "i-lucide:shield-check",
          id: "permissions",
        } satisfies OnboardingStepDefinition,
      ]
    : []),
  {
    component: ShortcutsStep,
    icon: "i-lucide:keyboard",
    id: "shortcuts",
  },
  {
    component: IgnoreAppsStep,
    icon: "i-lucide:ban",
    id: "ignoreApps",
  },
  {
    component: LegacyImportStep,
    icon: "i-lucide:database-backup",
    id: "legacyImport",
  },
  {
    component: DoneStep,
    icon: "i-lucide:badge-check",
    id: "done",
  },
];
