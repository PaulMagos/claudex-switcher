export const THEME_CHANGED_EVENT = "theme-changed";
export const THEME_STORAGE_KEY = "claudex-switcher-theme";

export type ThemeMode = "light" | "dark";

export function readStoredTheme(): ThemeMode {
  if (typeof window === "undefined") return "light";
  try {
    return window.localStorage.getItem(THEME_STORAGE_KEY) === "dark" ? "dark" : "light";
  } catch {
    return "light";
  }
}

export function applyTheme(theme: ThemeMode): void {
  document.documentElement.classList.toggle("dark", theme === "dark");
}

export function syncThemeFromStorage(): void {
  applyTheme(readStoredTheme());
}
