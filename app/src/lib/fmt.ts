import { lang, locale, t, type Key } from "./i18n.svelte";

export function bytes(n: number, digits = 1): string {
  if (!Number.isFinite(n) || n < 0) return "—";
  const ru = lang() === "ru";
  if (n < 1024) return `${Math.round(n)} ${ru ? "Б" : "B"}`;
  const units = ru ? ["КБ", "МБ", "ГБ", "ТБ"] : ["KB", "MB", "GB", "TB"];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  const num = v.toLocaleString(locale(), { maximumFractionDigits: v >= 100 ? 0 : digits, minimumFractionDigits: v >= 100 ? 0 : digits });
  return `${num} ${units[i]}`;
}

export function count(n: number): string {
  return n.toLocaleString(locale());
}

export function duration(ms: number): string {
  if (!Number.isFinite(ms) || ms < 0) return "—";
  const [s_, m_, h_] = [t("u.s"), t("u.min"), t("u.h")];
  const s = Math.round(ms / 1000);
  if (s < 1) return `${(ms / 1000).toLocaleString(locale(), { maximumFractionDigits: 1 })} ${s_}`;
  if (s < 60) return `${s} ${s_}`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m} ${m_} ${s % 60} ${s_}`;
  return `${Math.floor(m / 60)} ${h_} ${m % 60} ${m_}`;
}

export function date(ms: number): string {
  if (ms < 0) return "";
  const d = new Date(ms);
  return (
    d.toLocaleDateString(locale(), { year: "numeric", month: "2-digit", day: "2-digit" }) +
    " " +
    d.toLocaleTimeString(locale(), { hour: "2-digit", minute: "2-digit" })
  );
}

export function baseName(path: string): string {
  return path.replace(/[\\/]+$/, "").split(/[\\/]/).pop() ?? path;
}

export function dirName(path: string): string {
  const i = Math.max(path.lastIndexOf("\\"), path.lastIndexOf("/"));
  if (i < 0) return "";
  // Keep the backslash of a drive root ("C:\").
  return i === 2 && path[1] === ":" ? path.slice(0, 3) : path.slice(0, i);
}

export function ext(path: string): string {
  const b = baseName(path);
  const i = b.lastIndexOf(".");
  return i > 0 ? b.slice(i + 1).toLowerCase() : "";
}

const GROUPS: [string, string[]][] = [
  ["image", ["jpg", "jpeg", "png", "gif", "webp", "bmp", "svg", "ico", "heic", "avif", "tif", "tiff", "psd"]],
  ["video", ["mp4", "mkv", "webm", "avi", "mov", "m4v", "wmv"]],
  ["audio", ["mp3", "wav", "flac", "ogg", "opus", "m4a", "aac"]],
  ["archive", ["zip", "7z", "rar", "gz", "tgz", "xz", "zst", "bz2", "tar", "jar", "cab"]],
  ["code", ["js", "ts", "jsx", "tsx", "rs", "py", "c", "h", "cpp", "hpp", "cs", "java", "go", "rb", "php", "swift", "kt", "css", "scss", "html", "htm", "vue", "svelte", "sh", "ps1", "bat", "lua", "sql"]],
  ["text", ["txt", "md", "json", "xml", "yml", "yaml", "toml", "ini", "cfg", "log", "csv", "tsv", "rtf"]],
  ["doc", ["pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "odt", "ods", "odp", "epub"]],
  ["app", ["exe", "dll", "msi", "sys", "so", "dylib", "bin", "node", "pyd"]],
];

/** Kind of file for icons and the type column. */
export function fileKind(path: string): { kind: string; label: string } {
  const e = ext(path);
  for (const [kind, exts] of GROUPS) if (exts.includes(e)) return { kind, label: `${t(`kind.${kind}` as Key)} (.${e})` };
  return { kind: "file", label: e ? t("kind.extFile", { ext: e }) : t("kind.file") };
}
