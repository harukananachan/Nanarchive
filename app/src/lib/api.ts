// Bridge to the Rust side. Outside Tauri (plain `vite dev` in a browser) a
// small mock is used so the design can be checked without the native shell.

export type Preset = "store" | "fast" | "balanced" | "max" | "ultra";
export const PRESETS: Preset[] = ["store", "fast", "balanced", "max", "ultra"];

export type CoreInfo = {
  version: string;
  extension: string;
  format_version: number;
  presets: string[];
  threads: number;
};

export type Estimate = { files: number; dirs: number; bytes: number; sizes: number[] };

export type ArchiveView = {
  path: string;
  name: string;
  archive_size: number;
  total_size: number;
  files: number;
  dirs: number;
  blocks: number;
  methods: string[];
  paths: string[];
  sizes: number[];
  is_dir: boolean[];
  mtimes: number[];
};

export type Recent = { path: string; size: number; opened: number };

export type ProgressEvent = {
  job: number;
  phase: "scanning" | "compressing" | "finishing" | "extracting" | "testing";
  bytes_done: number;
  bytes_total: number;
  packed_bytes: number;
  files_done: number;
  files_total: number;
};

export type DoneEvent = {
  job: number;
  ok: boolean;
  cancelled: boolean;
  error: string | null;
  output: string;
  files: number;
  bytes_in: number;
  bytes_out: number;
  skipped: number;
  warnings: string[];
  elapsed_ms: number;
};

export type PreviewView = {
  kind: "image" | "text" | "none";
  mime: string;
  data: string;
  truncated: boolean;
  size: number;
};

export type DirEntry = { name: string; is_dir: boolean; size: number; mtime: number; drive: boolean };
export type DirView = { path: string; parent: string | null; entries: DirEntry[] };

export const inTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (inTauri) {
    const { invoke } = await import("@tauri-apps/api/core");
    return invoke<T>(cmd, args);
  }
  return mock(cmd, args ?? {}) as Promise<T>;
}

export const api = {
  coreInfo: () => call<CoreInfo>("core_info"),
  startupPaths: () => call<string[]>("startup_paths"),
  isArchive: (path: string) => call<boolean>("is_archive", { path }),
  listDir: (path: string) => call<DirView>("list_dir", { path }),
  homeDir: () => call<string>("home_dir"),
  suggestArchivePath: (inputs: string[]) => call<string>("suggest_archive_path", { inputs }),
  suggestExtractDir: (archive: string) => call<string>("suggest_extract_dir", { archive }),
  estimate: (inputs: string[]) => call<Estimate>("estimate", { inputs }),
  openArchive: (path: string) => call<ArchiveView>("open_archive", { path }),
  recentList: () => call<Recent[]>("recent_list"),
  recentRemove: (path: string) => call<void>("recent_remove", { path }),
  createArchive: (job: number, dest: string, inputs: string[], preset: Preset, overwrite: boolean) =>
    call<void>("create_archive", { job, dest, inputs, preset, overwrite }),
  extractArchive: (job: number, archive: string, dest: string, only: string[], overwrite: boolean) =>
    call<void>("extract_archive", { job, archive, dest, only, overwrite }),
  testArchive: (job: number, archive: string) => call<void>("test_archive", { job, archive }),
  cancelJob: (job: number) => call<void>("cancel_job", { job }),
  preview: (archive: string, entry: string) => call<PreviewView>("preview", { archive, entry }),
};

// --- Events ----------------------------------------------------------------

type Handler<T> = (payload: T) => void;
const mockListeners: Record<string, Handler<any>[]> = {};

export async function listen<T>(event: string, handler: Handler<T>): Promise<() => void> {
  if (inTauri) {
    const { listen } = await import("@tauri-apps/api/event");
    return listen<T>(event, (e) => handler(e.payload));
  }
  (mockListeners[event] ??= []).push(handler);
  return () => {
    mockListeners[event] = mockListeners[event].filter((h) => h !== handler);
  };
}

/** Files dropped on the window (paths), plus hover state. */
export async function onFileDrop(
  drop: (paths: string[]) => void,
  hover: (over: boolean) => void,
): Promise<() => void> {
  if (!inTauri) return () => {};
  const { getCurrentWebview } = await import("@tauri-apps/api/webview");
  return getCurrentWebview().onDragDropEvent((e) => {
    const p = e.payload;
    if (p.type === "enter" || p.type === "over") hover(true);
    else if (p.type === "leave") hover(false);
    else if (p.type === "drop") {
      hover(false);
      if (p.paths.length) drop(p.paths);
    }
  });
}

// --- Dialogs and shell -----------------------------------------------------

export async function pickFiles(): Promise<string[]> {
  if (!inTauri) return ["C:\\Users\\demo\\Pictures\\holiday.jpg", "C:\\Users\\demo\\Documents\\notes.txt"];
  const { open } = await import("@tauri-apps/plugin-dialog");
  const r = await open({ multiple: true, title: "Choose files to compress" });
  return r ? (Array.isArray(r) ? r : [r]) : [];
}

export async function pickFolder(title = "Choose a folder"): Promise<string | null> {
  if (!inTauri) return "C:\\Users\\demo\\Projects\\website";
  const { open } = await import("@tauri-apps/plugin-dialog");
  const r = await open({ directory: true, title });
  return typeof r === "string" ? r : null;
}

export async function pickArchive(): Promise<string | null> {
  if (!inTauri) return "C:\\Users\\demo\\Downloads\\website.nax";
  const { open } = await import("@tauri-apps/plugin-dialog");
  const r = await open({
    title: "Open archive",
    filters: [{ name: "Nanarchive", extensions: ["nax"] }],
  });
  return typeof r === "string" ? r : null;
}

export async function pickSavePath(defaultPath: string): Promise<string | null> {
  if (!inTauri) return defaultPath;
  const { save } = await import("@tauri-apps/plugin-dialog");
  return save({
    title: "Save archive as",
    defaultPath,
    filters: [{ name: "Nanarchive", extensions: ["nax"] }],
  });
}

export async function reveal(path: string) {
  if (!inTauri) return;
  const { revealItemInDir } = await import("@tauri-apps/plugin-opener");
  await revealItemInDir(path);
}

/** Opens a file on disk with its default Windows program. */
export async function openWithSystem(path: string) {
  if (!inTauri) return;
  const { openPath } = await import("@tauri-apps/plugin-opener");
  await openPath(path);
}

export const win = {
  async minimize() {
    if (!inTauri) return;
    const { getCurrentWindow } = await import("@tauri-apps/api/window");
    await getCurrentWindow().minimize();
  },
  async toggleMaximize() {
    if (!inTauri) return;
    const { getCurrentWindow } = await import("@tauri-apps/api/window");
    await getCurrentWindow().toggleMaximize();
  },
  async close() {
    if (!inTauri) return;
    const { getCurrentWindow } = await import("@tauri-apps/api/window");
    await getCurrentWindow().close();
  },
};

// --- Mock (browser preview only) ---------------------------------------------

function emit(event: string, payload: unknown) {
  for (const h of mockListeners[event] ?? []) h(payload);
}

function mockArchive(path: string): ArchiveView {
  const paths: string[] = [];
  const sizes: number[] = [];
  const is_dir: boolean[] = [];
  const mtimes: number[] = [];
  const add = (p: string, size: number, dir = false) => {
    paths.push(p);
    sizes.push(size);
    is_dir.push(dir);
    mtimes.push(Date.now() - Math.floor(Math.random() * 9e9));
  };
  add("website", 0, true);
  add("website/src", 0, true);
  add("website/assets", 0, true);
  add("website/assets/photos", 0, true);
  add("website/index.html", 18_211);
  add("website/README.md", 4_120);
  add("website/package.json", 1_532);
  for (let i = 0; i < 1200; i++) add(`website/src/module-${String(i).padStart(4, "0")}.ts`, 800 + ((i * 7919) % 40_000));
  for (let i = 0; i < 40; i++) add(`website/assets/photos/IMG_${2000 + i}.jpg`, 2_400_000 + i * 31_000);
  const total = sizes.reduce((a, b) => a + b, 0);
  return {
    path,
    name: path.split(/[\\/]/).pop() ?? path,
    archive_size: Math.round(total * 0.42),
    total_size: total,
    files: paths.length - 4,
    dirs: 4,
    blocks: 9,
    methods: ["lzma2", "recompress"],
    paths,
    sizes,
    is_dir,
    mtimes,
  };
}

let mockRecent: Recent[] = [
  { path: "C:\\Users\\demo\\Downloads\\website.nax", size: 41_200_000, opened: Date.now() - 3_600_000 },
  { path: "D:\\Backups\\photos-2026.nax", size: 1_840_000_000, opened: Date.now() - 86_400_000 * 2 },
  { path: "C:\\Users\\demo\\Desktop\\thesis.nax", size: 6_300_000, opened: Date.now() - 86_400_000 * 9 },
];

function mockJob(job: number, phase: ProgressEvent["phase"], total: number, ratio: number, output: string) {
  let done = 0;
  const files = 1244;
  const t0 = Date.now();
  const tick = setInterval(() => {
    done = Math.min(total, done + total / 60);
    emit("job-progress", {
      job,
      phase,
      bytes_done: done,
      bytes_total: total,
      packed_bytes: done * ratio,
      files_done: Math.round((files * done) / total),
      files_total: files,
    });
    if (done >= total) {
      clearInterval(tick);
      emit("job-done", {
        job,
        ok: true,
        cancelled: false,
        error: null,
        output,
        files,
        bytes_in: total,
        bytes_out: Math.round(total * ratio),
        skipped: 0,
        warnings: [],
        elapsed_ms: Date.now() - t0,
      });
    }
  }, 50);
  mockJobs.set(job, tick);
}
const mockJobs = new Map<number, ReturnType<typeof setInterval>>();

async function mock(cmd: string, a: Record<string, any>): Promise<unknown> {
  await new Promise((r) => setTimeout(r, 60));
  switch (cmd) {
    case "core_info":
      return { version: "0.1.0", extension: "nax", format_version: 1, presets: PRESETS, threads: 28 };
    case "startup_paths":
      return [];
    case "is_archive":
      return String(a.path).toLowerCase().endsWith(".nax");
    case "home_dir":
      return "C:\\Users\\demo";
    case "list_dir": {
      const p: string = a.path;
      if (!p) return { path: "", parent: null, entries: ["C:\\", "D:\\"].map((name) => ({ name, is_dir: true, size: 0, mtime: -1, drive: true })) };
      const t = Date.now();
      const e = (name: string, size = 0, dir = false) => ({ name, is_dir: dir, size, mtime: t - ((name.length * 7919) % 90) * 86_400_000, drive: false });
      const entries = [
        e("Desktop", 0, true), e("Documents", 0, true), e("Downloads", 0, true), e("Music", 0, true), e("Pictures", 0, true), e("Projects", 0, true),
        e("website.nax", 41_200_000), e("holiday.jpg", 3_400_000), e("notes.txt", 12_400), e("report.docx", 284_000), e("setup.exe", 18_900_000), e("song.mp3", 6_100_000),
      ];
      const parent = p.length <= 3 ? "" : p.replace(/\\[^\\]*$/, "") || "";
      return { path: p, parent: parent.length === 2 ? parent + "\\" : parent, entries };
    }
    case "suggest_archive_path": {
      const first: string = a.inputs[0];
      const dir = first.replace(/[\\/][^\\/]*$/, "");
      const name = a.inputs.length === 1 ? first.split(/[\\/]/).pop()!.replace(/\.[^.]*$/, "") : dir.split(/[\\/]/).pop();
      return `${dir}\\${name}.nax`;
    }
    case "suggest_extract_dir":
      return String(a.archive).replace(/\.nax$/i, "");
    case "estimate": {
      const bytes = 152_000_000 * a.inputs.length;
      return { files: 1244 * a.inputs.length, dirs: 31, bytes, sizes: [1.0, 0.31, 0.215, 0.205, 0.196].map((f) => Math.round(bytes * f)) };
    }
    case "open_archive":
      return mockArchive(a.path);
    case "recent_list":
      return mockRecent;
    case "recent_remove":
      mockRecent = mockRecent.filter((r) => r.path !== a.path);
      return;
    case "create_archive":
      mockJob(a.job, "compressing", 152_000_000, 0.21, a.dest);
      return;
    case "extract_archive":
      mockJob(a.job, "extracting", 98_000_000, 1, a.dest);
      return;
    case "test_archive":
      mockJob(a.job, "testing", 98_000_000, 1, a.archive);
      return;
    case "cancel_job": {
      clearInterval(mockJobs.get(a.job));
      emit("job-done", { job: a.job, ok: false, cancelled: true, error: "cancelled", output: "", files: 0, bytes_in: 0, bytes_out: 0, skipped: 0, warnings: [], elapsed_ms: 0 });
      return;
    }
    case "preview":
      if (/\.(ts|md|json|html)$/.test(a.entry))
        return { kind: "text", mime: "text/plain", data: `// ${a.entry}\nexport function hello(name: string) {\n  return \`Hello, \${name}!\`;\n}\n`, truncated: false, size: 120 };
      return { kind: "none", mime: "", data: "", truncated: false, size: 0 };
  }
  throw new Error(`mock: unknown command ${cmd}`);
}
