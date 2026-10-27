import { create } from "zustand";
import { persist } from "zustand/middleware";
import type { AccentColor } from "./useThemeStore";

export interface LauncherTheme {
  id: string;
  name: string;
  accentColor: AccentColor;
  backgroundImage?: string; // Path to background image (only for play screen)
  /** Optional i18n key for the display name; falls back to `name`. */
  nameKey?: string;
  /** Optional CSS custom properties (e.g. "--background": "256 60% 5%") applied to :root while the theme is active. */
  cssVars?: Record<string, string>;
  /** Selected automatically on fresh installs (never overrides a stored choice). */
  defaultForFreshInstall?: boolean;
  unlockRequirement?: {
    type: "advent-door";
    day: number;
  };
}

export const LAUNCHER_THEMES: Record<string, LauncherTheme> = {
  christmas_theme: {
    id: "christmas_theme",
    name: "Christmas Theme",
    accentColor: {
      name: "Christmas Blue",
      value: "#4A90D9",
      hoverValue: "#3A7BC8",
      light: "#6BA3E3",
      dark: "#2E5A8A",
      shadowValue: "rgba(74, 144, 217, 0.5)",
      isCustom: true,
    },
    backgroundImage: "/themes/christmas_theme.png",
    unlockRequirement: {
      type: "advent-door",
      day: 2,
    },
  },
  lu_hub: {
    id: "lu_hub",
    name: "Lu Hub",
    nameKey: "settings.theme.lu_hub",
    accentColor: {
      name: "Lu Hub Violet",
      value: "#7c3aed", // white on accent 5.7:1
      hoverValue: "#6d28d9", // white on hover 7.1:1
      light: "#a78bfa", // accent-as-text on night background 7.2:1
      dark: "#5b21b6",
      shadowValue: "rgba(167, 139, 250, 0.5)", // lighter lilac glow
      isCustom: true,
    },
    backgroundImage: "/themes/lu_hub.svg",
    defaultForFreshInstall: true,
    // Night-violet surfaces; text stays near-white (>= 12:1 on background, white-on-accent >= 4.5:1)
    cssVars: {
      "--background": "257 60% 6%",
      "--foreground": "260 100% 98%",
      "--card": "257 45% 11%",
      "--popover": "257 45% 11%",
      "--secondary": "258 30% 20%",
      "--muted": "258 30% 20%",
      "--muted-foreground": "256 40% 76%",
      "--border": "258 32% 26%",
      "--input": "258 32% 26%",
      "--ring": "258 90% 66%",
      "--text-secondary": "#c4b5fd",
      "--text-muted": "#a899d6",
    },
  },
};

interface LauncherThemeState {
  // Currently selected theme (null means no theme, use custom accent color)
  selectedThemeId: string | null;
  // Track which advent calendar doors have been opened (persisted)
  openedAdventDoors: number[];
  // Original accent color before theme was applied (to restore when deselecting)
  originalAccentColor: AccentColor | null;
  // Actions
  selectTheme: (themeId: string | null) => void;
  markAdventDoorOpened: (day: number) => void;
  isThemeUnlocked: (themeId: string) => boolean;
  setOriginalAccentColor: (color: AccentColor | null) => void;
  getSelectedTheme: () => LauncherTheme | null;
}

// Initial value only applies when nothing is persisted yet; stored choices (including "no theme") win on rehydrate.
function freshInstallThemeId(): string | null {
  return Object.values(LAUNCHER_THEMES).find((t) => t.defaultForFreshInstall)?.id ?? null;
}

export const useLauncherThemeStore = create<LauncherThemeState>()(
  persist(
    (set, get) => ({
      selectedThemeId: freshInstallThemeId(),
      openedAdventDoors: [],
      originalAccentColor: null,

      selectTheme: (themeId: string | null) => {
        set({ selectedThemeId: themeId });
      },

      markAdventDoorOpened: (day: number) => {
        set((state) => {
          if (state.openedAdventDoors.includes(day)) {
            return state;
          }
          return {
            openedAdventDoors: [...state.openedAdventDoors, day].sort((a, b) => a - b),
          };
        });
      },

      isThemeUnlocked: (themeId: string) => {
        const theme = LAUNCHER_THEMES[themeId];
        if (!theme) return false;

        // No unlock requirement means it's always unlocked
        if (!theme.unlockRequirement) return true;

        const { openedAdventDoors } = get();

        if (theme.unlockRequirement.type === "advent-door") {
          return openedAdventDoors.includes(theme.unlockRequirement.day);
        }

        return false;
      },

      setOriginalAccentColor: (color: AccentColor | null) => {
        set({ originalAccentColor: color });
      },

      getSelectedTheme: () => {
        const { selectedThemeId } = get();
        if (!selectedThemeId) return null;
        return LAUNCHER_THEMES[selectedThemeId] || null;
      },
    }),
    {
      name: "launcher-theme-storage",
    }
  )
);

