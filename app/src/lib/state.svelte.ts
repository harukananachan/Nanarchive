import type { Preset } from "./api";

// --- Settings (saved in this computer's WebView storage only) ---------------

export type Theme = "system" | "light" | "dark";
export const ACCENTS = {
  lavender: { name: "Lavender", color: "#8b6cf6" },
  sky: { name: "Sky", color: "#4f9cf0" },
  mint: { name: "Mint", color: "#2fb38a" },
  rose: { name: "Rose", color: "#ec6a92" },
  peach: { name: "Peach", color: "#f08a4b" },
} as const;
export type Accent = keyof typeof ACCENTS;

export type LangSetting = "auto" | "en" | "ru";
type Saved = { theme: Theme; accent: Accent; preset: Preset; motion: boolean; labels: boolean; lastDir: string; lang: LangSetting; explorer: boolean; recovery: number; splitMb: number; updateUrl: string; autoUpdate: boolean };
const KEY = "nanarchive.settings";
const defaults: Saved = { theme: "system", accent: "lavender", preset: "balanced", motion: true, labels: true, lastDir: "", lang: "auto", explorer: true, recovery: 0, splitMb: 0, updateUrl: "", autoUpdate: true };

function load(): Saved {
  try {
    const raw = localStorage.getItem(KEY);
    if (raw) return { ...defaults, ...JSON.parse(raw) };
  } catch {}
  return { ...defaults };
}

export const settings = $state<Saved>(load());

export function saveSettings() {
  try {
    localStorage.setItem(KEY, JSON.stringify(settings));
  } catch {}
}

// --- Toasts -------------------------------------------------------------------

export type Toast = { id: number; text: string; kind: "info" | "error" | "ok" };
export const toasts = $state<Toast[]>([]);
let nextToast = 1;

export function toast(text: string, kind: Toast["kind"] = "info", ms = 4200) {
  const id = nextToast++;
  toasts.push({ id, text, kind });
  setTimeout(() => dismiss(id), ms);
}

export function dismiss(id: number) {
  const i = toasts.findIndex((t) => t.id === id);
  if (i >= 0) toasts.splice(i, 1);
}

// --- Job ids --------------------------------------------------------------------

let nextJob = 1;
export const newJob = () => nextJob++;
