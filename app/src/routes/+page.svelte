<script lang="ts">
  import { onMount, tick } from "svelte";
  import MenuBar, { type Menu, type MenuItem } from "$lib/MenuBar.svelte";
  import FileList, { type Row } from "$lib/FileList.svelte";
  import FileIcon from "$lib/FileIcon.svelte";
  import ToolIcon from "$lib/ToolIcon.svelte";
  import Icon from "$lib/Icon.svelte";
  import AddDialog from "$lib/AddDialog.svelte";
  import ExtractDialog from "$lib/ExtractDialog.svelte";
  import InfoDialog from "$lib/InfoDialog.svelte";
  import Settings from "$lib/Settings.svelte";
  import Preview from "$lib/Preview.svelte";
  import PasswordDialog from "$lib/PasswordDialog.svelte";
  import Progress, { type JobKind } from "$lib/Progress.svelte";
  import Toasts from "$lib/Toasts.svelte";
  import {
    api,
    listen,
    onFileDrop,
    openWithSystem,
    pickArchive,
    pickFiles,
    reveal,
    win,
    type ArchiveView,
    type CoreInfo,
    type DirView,
    type DoneEvent,
    type Preset,
    type ProgressEvent,
    type Protection,
    type Recent,
  } from "$lib/api";
  import { baseName, bytes, count, dirName, ext, fileKind } from "$lib/fmt";
  import { buildTree, type Node } from "$lib/tree";
  import { ACCENTS, newJob, saveSettings, settings, toast } from "$lib/state.svelte";
  import { lang, methodNames, t } from "$lib/i18n.svelte";
  import { syncExplorer } from "$lib/explorer";

  // --- Where we are -----------------------------------------------------------
  type Loc =
    | { kind: "disk"; dir: DirView }
    | { kind: "arc"; archive: ArchiveView; nodes: Node[]; cwd: number };
  let loc = $state<Loc | null>(null);
  let selected = $state(new Set<string>());
  let query = $state("");
  let debounced = $state("");
  let addr = $state("");
  let busy = $state(false);
  let info = $state<CoreInfo | null>(null);
  let recent = $state<Recent[]>([]);
  let hovering = $state(false);
  let list = $state<FileList>();
  let findEl = $state<HTMLInputElement>();

  // --- Dialogs ----------------------------------------------------------------
  let addFor = $state<string[] | null>(null);
  let extractFor = $state<{ archive: string; only: string[] } | null>(null);
  let infoFor = $state<ArchiveView | null>(null);
  let settingsOpen = $state(false);
  let previewing = $state<{ archive: string; node: Node } | null>(null);
  let ctx = $state<{ x: number; y: number; items: MenuItem[] } | null>(null);
  let askPw = $state<{ name: string; wrong: boolean; resolve: (pw: string | null) => void } | null>(null);

  type Job = { id: number; kind: JobKind; title: string; progress: ProgressEvent | null; done: DoneEvent | null };
  let job = $state<Job | null>(null);
  /** A queued action is getting ready (e.g. picking a folder name). */
  let starting = $state(false);
  const blocked = $derived(!!(addFor || extractFor || infoFor || settingsOpen || previewing || job || starting || askPw));

  // --- Theme ------------------------------------------------------------------
  $effect(() => {
    const root = document.documentElement;
    if (settings.theme === "system") delete root.dataset.theme;
    else root.dataset.theme = settings.theme;
    root.dataset.motion = settings.motion ? "on" : "off";
    root.style.setProperty("--accent", ACCENTS[settings.accent]?.color ?? ACCENTS.lavender.color);
    root.lang = lang();
  });

  // --- Paths ------------------------------------------------------------------
  const join = (dir: string, name: string) => (dir.endsWith("\\") || dir.endsWith("/") ? dir + name : `${dir}\\${name}`);
  const isNaxName = (p: string) => ext(p) === "nax";
  /** Archives Nanarchive opens itself (.docx and friends are ZIPs too, but open in their own apps). */
  const isArcName = (p: string) => ["nax", "zip", "jar", "7z", "rar"].includes(ext(p));

  function typeOf(name: string): { icon: string; type: string } {
    if (isNaxName(name)) return { icon: "nax", type: t("type.nax") };
    const k = fileKind(name);
    return { icon: k.kind, type: k.label };
  }

  function arcPath(a: ArchiveView, inner: string) {
    return inner ? `${a.path}\\${inner.replaceAll("/", "\\")}` : a.path;
  }

  // --- Navigation -------------------------------------------------------------
  async function gotoDisk(path: string, select?: string) {
    busy = true;
    try {
      const dir = await api.listDir(path);
      loc = { kind: "disk", dir };
      addr = dir.path || t("thisPc");
      query = debounced = "";
      selected = select ? new Set([select]) : new Set();
      if (dir.path) {
        settings.lastDir = dir.path;
        saveSettings();
      }
    } catch (e) {
      toast(String(e), "error", 6000);
      addr = currentAddr();
    } finally {
      busy = false;
    }
  }

  // --- Passwords --------------------------------------------------------------
  function askPassword(name: string, wrong: boolean): Promise<string | null> {
    return new Promise((resolve) => (askPw = { name, wrong, resolve }));
  }

  function answerPassword(pw: string | null) {
    const a = askPw;
    askPw = null;
    a?.resolve(pw);
  }

  /** Opens an archive, asking for its password when it has one. Null if the
   *  user gave up. The app remembers the key until it closes (memory only). */
  async function unlockArchive(path: string): Promise<ArchiveView | null> {
    let pw: string | undefined;
    for (;;) {
      try {
        return await api.openArchive(path, pw);
      } catch (e) {
        const s = String(e);
        if (s !== "password_required" && s !== "wrong_password") throw e;
        const got = await askPassword(baseName(path), s === "wrong_password");
        if (got === null) return null;
        pw = got;
      }
    }
  }

  async function openArchive(path: string, inner = "") {
    busy = true;
    try {
      const archive = await unlockArchive(path);
      if (!archive) return;
      const nodes = buildTree(archive);
      let cwd = 0;
      if (inner) {
        const id = nodes.findIndex((n) => n.dir && n.path.toLowerCase() === inner.toLowerCase());
        if (id > 0) cwd = id;
      }
      loc = { kind: "arc", archive, nodes, cwd };
      addr = arcPath(archive, nodes[cwd].path);
      query = debounced = "";
      selected = new Set();
      refreshRecent();
    } catch (e) {
      toast(t("err.open", { name: baseName(path), e: String(e) }), "error", 6000);
    } finally {
      busy = false;
    }
  }

  function enterFolder(id: number) {
    if (loc?.kind !== "arc") return;
    loc.cwd = id;
    addr = arcPath(loc.archive, loc.nodes[id].path);
    query = debounced = "";
    selected = new Set();
  }

  function up() {
    if (!loc) return;
    if (loc.kind === "disk") {
      if (loc.dir.parent !== null) gotoDisk(loc.dir.parent, loc.dir.path);
    } else if (query) {
      query = debounced = "";
    } else if (loc.cwd > 0) {
      const from = loc.cwd;
      enterFolder(loc.nodes[from].parent);
      selected = new Set([`n${from}`]);
    } else {
      gotoDisk(dirName(loc.archive.path) || "", loc.archive.path);
    }
  }

  function currentAddr() {
    if (!loc) return "";
    return loc.kind === "disk" ? loc.dir.path || t("thisPc") : arcPath(loc.archive, loc.nodes[loc.cwd].path);
  }

  async function goAddress() {
    const a = addr.trim().replace(/^"|"$/g, "");
    if (!a || a === t("thisPc")) return gotoDisk("");
    const m = a.match(/^(.*?\.(?:nax|zip|jar|7z|rar))(?:[\\/](.*))?$/i);
    if (m && (await api.isArchive(m[1]))) return openArchive(m[1], (m[2] ?? "").replaceAll("\\", "/").replace(/\/$/, ""));
    gotoDisk(a);
  }

  function refresh() {
    if (loc?.kind === "disk") gotoDisk(loc.dir.path, [...selected][0]);
    else if (loc?.kind === "arc") openArchive(loc.archive.path, loc.nodes[loc.cwd].path);
  }

  // --- Rows -------------------------------------------------------------------
  $effect(() => {
    const q = query;
    const t = setTimeout(() => (debounced = q.trim().toLowerCase()), 120);
    return () => clearTimeout(t);
  });

  const UP = $derived<Row>({ key: "..", name: "..", dir: true, size: 0, mtime: -1, icon: "up", type: t("type.up") });

  const rows = $derived.by((): Row[] => {
    if (!loc) return [];
    const q = debounced;
    if (loc.kind === "disk") {
      const d = loc.dir;
      const out: Row[] = d.parent !== null ? [UP] : [];
      for (const e of d.entries) {
        if (q && !e.name.toLowerCase().includes(q)) continue;
        const k = e.drive ? { icon: "drive", type: t("type.disk") } : e.is_dir ? { icon: "folder", type: t("type.folder") } : typeOf(e.name);
        out.push({ key: e.drive ? e.name : join(d.path, e.name), name: e.name, dir: e.is_dir, size: e.size, mtime: e.mtime, ...k });
      }
      return out;
    }
    const { nodes, cwd } = loc;
    const ids = q ? nodes.map((_, i) => i).filter((i) => i > 0 && nodes[i].lower.includes(q)) : nodes[cwd].kids;
    const out: Row[] = q ? [] : [UP];
    for (const id of ids) {
      const n = nodes[id];
      const k = n.dir ? { icon: "folder", type: t("type.folder") } : typeOf(n.name);
      const where = q && n.parent > 0 ? nodes[n.parent].path : undefined;
      out.push({ key: `n${id}`, name: n.name, dir: n.dir, size: n.size, mtime: n.mtime, where, ...k });
    }
    return out;
  });

  const nodeOf = (key: string) => (loc?.kind === "arc" && key.startsWith("n") ? loc.nodes[Number(key.slice(1))] : null);
  const selRows = $derived(rows.filter((r) => selected.has(r.key)));
  const selNax = $derived(loc?.kind === "disk" ? selRows.find((r) => !r.dir && isArcName(r.name)) : undefined);

  function activate(r: Row) {
    if (r.icon === "up") return up();
    if (!loc) return;
    if (loc.kind === "disk") {
      if (r.dir) gotoDisk(r.key);
      else if (isArcName(r.name)) openArchive(r.key);
      else openWithSystem(r.key).catch((e) => toast(String(e), "error"));
    } else {
      const n = nodeOf(r.key);
      if (!n) return;
      if (n.dir) enterFolder(Number(r.key.slice(1)));
      else previewing = { archive: loc.archive.path, node: n };
    }
  }

  // --- Commands ---------------------------------------------------------------
  const can = $derived({
    add: loc?.kind === "disk",
    extract: loc?.kind === "arc" || !!selNax,
    test: loc?.kind === "arc" || !!selNax,
    view: selRows.length > 0,
    info: loc?.kind === "arc" || !!selNax,
    repair: loc?.kind === "arc" ? loc.archive.format === "nax" : !!selNax && isNaxName(selNax.name),
  });

  async function cmdAdd(paths?: string[]) {
    let list = paths ?? selRows.filter((r) => !r.dir || r.icon === "folder").map((r) => r.key);
    if (!list.length) list = await pickFiles();
    if (list.length) addFor = list;
  }

  async function cmdExtract() {
    if (loc?.kind === "arc") {
      const only = selRows.map((r) => nodeOf(r.key)?.path).filter((p): p is string => !!p);
      extractFor = { archive: loc.archive.path, only };
    } else if (selNax) {
      const path = selNax.key;
      if (await unlockQuietly(path)) extractFor = { archive: path, only: [] };
    }
  }

  async function cmdTest() {
    const path = loc?.kind === "arc" ? loc.archive.path : selNax?.key;
    if (path && (await unlockQuietly(path))) startJob("test", baseName(path), (id) => api.testArchive(id, path));
  }

  function cmdRepair() {
    const path = loc?.kind === "arc" ? loc.archive.path : selNax?.key;
    if (path) startJob("repair", baseName(path), (id) => api.repairArchive(id, path));
  }

  /** Unlock before a job; shows errors as a toast. */
  async function unlockQuietly(path: string) {
    try {
      return !!(await unlockArchive(path));
    } catch (e) {
      toast(t("err.open", { name: baseName(path), e: String(e) }), "error", 6000);
      return false;
    }
  }

  function cmdView() {
    const r = selRows[0];
    if (r) activate(r);
  }

  async function cmdInfo() {
    if (loc?.kind === "arc") infoFor = loc.archive;
    else if (selNax) {
      try {
        infoFor = await unlockArchive(selNax.key);
      } catch (e) {
        toast(String(e), "error");
      }
    }
  }

  function cmdFind() {
    findEl?.focus();
    findEl?.select();
  }

  async function cmdOpen() {
    const p = await pickArchive();
    if (p) openArchive(p);
  }

  async function handlePaths(paths: string[]) {
    if (blocked || !paths.length) return;
    if (paths.length === 1 && (await api.isArchive(paths[0]))) return openArchive(paths[0]);
    addFor = paths;
  }

  async function refreshRecent() {
    try {
      recent = await api.recentList();
    } catch {}
  }

  // --- Jobs -------------------------------------------------------------------
  function startJob(kind: JobKind, title: string, run: (id: number) => Promise<void>) {
    const id = newJob();
    job = { id, kind, title, progress: null, done: null };
    run(id).catch((e) => {
      if (job?.id === id)
        job.done = { job: id, ok: false, cancelled: false, error: String(e), output: "", files: 0, bytes_in: 0, bytes_out: 0, skipped: 0, warnings: [], elapsed_ms: 0, parts: 0, note: null };
    });
  }

  function startAdd(dest: string, preset: Preset, overwrite: boolean, protection: Protection) {
    const inputs = addFor ?? [];
    addFor = null;
    startJob("create", baseName(dest), (id) => api.createArchive(id, dest, inputs, preset, overwrite, protection));
  }

  function startExtract(dest: string, overwrite: boolean) {
    const e = extractFor;
    extractFor = null;
    if (!e) return;
    const what = e.only.length ? t("job.someFrom", { n: count(e.only.length), name: baseName(e.archive) }) : baseName(e.archive);
    startJob("extract", `${what} → ${dest}`, (id) => api.extractArchive(id, e.archive, dest, e.only, overwrite));
  }

  function closeJob() {
    const j = job;
    job = null;
    if (!j?.done?.ok) return;
    // Show the new archive or folder if it landed in the folder on screen.
    if (loc?.kind === "disk" && dirName(j.done.output).toLowerCase() === loc.dir.path.replace(/\\$/, "").toLowerCase())
      gotoDisk(loc.dir.path, j.done.output);
    else if (loc?.kind === "disk") refresh();
    refreshRecent();
  }

  async function openResult() {
    const out = job?.done?.output;
    job = null;
    if (quick) await leaveQuick();
    if (out) await openArchive(out);
  }

  // --- Quick actions (Explorer's right-click menu) ---------------------------
  // Explorer starts the app once per selected file, e.g. `--extract-here a.nax`.
  // Later copies hand their arguments to this one ("args" event); arguments
  // that arrive within a moment of each other are merged into one action.
  const QUICK = ["--add", "--add-quick", "--extract-here", "--extract-to", "--test"];
  /** Compact window that closes by itself when its work is done. */
  let quick = $state(false);
  let quickArmed = $state(false);
  let later = $state<(() => void)[]>([]);
  let gather: { flag: string; paths: string[] } | null = null;
  let gatherTimer: ReturnType<typeof setTimeout> | undefined;

  function whenFree(fn: () => void) {
    if (!blocked && !later.length) fn();
    else later.push(fn);
  }

  // Run the next queued action once the window is free.
  $effect(() => {
    if (!blocked && later.length) later.shift()!();
  });

  // A quick window with nothing left to do closes.
  $effect(() => {
    if (quick && quickArmed && !blocked && !later.length && !busy) win.close();
  });

  function receiveArgs(args: string[]) {
    const flag = args.find((a) => a.startsWith("--")) ?? "";
    const paths = args.filter((a) => !a.startsWith("--"));
    if (!paths.length) return;
    if (gather && gather.flag !== flag) flushArgs();
    gather ??= { flag, paths: [] };
    gather.paths.push(...paths);
    clearTimeout(gatherTimer);
    gatherTimer = setTimeout(flushArgs, 350);
  }

  async function flushArgs() {
    const g = gather;
    gather = null;
    clearTimeout(gatherTimer);
    if (!g) return;
    const paths = [...new Set(g.paths)];
    try {
      switch (g.flag) {
        case "--add":
          whenFree(() => (addFor = paths));
          break;
        case "--add-quick": {
          const dest = await api.suggestArchivePath(paths);
          const preset = settings.preset;
          const none: Protection = { password: null, recovery: settings.recovery, volume: null };
          whenFree(() => startJob("create", baseName(dest), (id) => api.createArchive(id, dest, paths, preset, false, none)));
          break;
        }
        case "--extract-here":
        case "--extract-to":
          for (const p of paths) {
            // The folder name is picked when the job starts, so two "photos" archives get "photos" and "photos (2)".
            whenFree(async () => {
              starting = true;
              try {
                if (!(await unlockArchive(p))) return;
                const dest = g.flag === "--extract-here" ? dirName(p) : await api.suggestExtractDir(p);
                startJob("extract", `${baseName(p)} → ${dest}`, (id) => api.extractArchive(id, p, dest, [], false));
              } catch (e) {
                toast(String(e), "error", 6000);
              } finally {
                starting = false;
              }
            });
          }
          break;
        case "--test":
          for (const p of paths)
            whenFree(async () => {
              starting = true;
              try {
                if (await unlockArchive(p)) startJob("test", baseName(p), (id) => api.testArchive(id, p));
              } catch (e) {
                toast(String(e), "error", 6000);
              } finally {
                starting = false;
              }
            });
          break;
        default:
          if (quick) await leaveQuick();
          if (paths.length === 1 && (await api.isArchive(paths[0]))) whenFree(() => openArchive(paths[0]));
          else whenFree(() => (addFor = paths));
      }
    } catch (e) {
      toast(String(e), "error", 6000);
    }
    quickArmed = true;
  }

  /** Grows the quick window into the normal file manager. */
  async function leaveQuick() {
    quick = false;
    await api.setWindowMode(false, false);
    if (!loc) await gotoDisk("");
  }

  // --- Menus ------------------------------------------------------------------
  const menus = $derived<Menu[]>([
    {
      label: t("menu.file"),
      items: [
        { label: t("m.openArchive"), shortcut: "Ctrl+O", action: cmdOpen },
        ...(recent.length ? [{ sep: true } as const] : []),
        ...recent.slice(0, 8).map((r) => ({ label: baseName(r.path), shortcut: bytes(r.size), action: () => openArchive(r.path) })),
        { sep: true },
        { label: t("m.home"), action: async () => gotoDisk(await api.homeDir()) },
        { label: t("m.thisPc"), action: () => gotoDisk("") },
        { label: t("m.refresh"), shortcut: "F5", action: refresh },
        { sep: true },
        { label: t("m.selectAll"), shortcut: "Ctrl+A", action: () => (selected = new Set(rows.filter((r) => r.icon !== "up").map((r) => r.key))) },
        { sep: true },
        { label: t("m.exit"), action: win.close },
      ],
    },
    {
      label: t("menu.commands"),
      items: [
        { label: t("m.add"), shortcut: "Alt+A", action: () => cmdAdd(), disabled: !can.add },
        { label: t("m.extract"), shortcut: "Alt+E", action: cmdExtract, disabled: !can.extract },
        { label: t("m.test"), shortcut: "Alt+T", action: cmdTest, disabled: !can.test },
        { label: t("m.repair"), shortcut: "Alt+R", action: cmdRepair, disabled: !can.repair },
        { label: t("m.view"), shortcut: "Alt+V", action: cmdView, disabled: !can.view },
        { sep: true },
        { label: t("m.find"), shortcut: "Ctrl+F", action: cmdFind },
        { label: t("m.info"), shortcut: "Alt+I", action: cmdInfo, disabled: !can.info },
      ],
    },
    {
      label: t("menu.options"),
      items: [
        { label: t("m.settings"), action: () => (settingsOpen = true) },
        { sep: true },
        { label: t("m.light"), checked: settings.theme === "light", action: () => ((settings.theme = "light"), saveSettings()) },
        { label: t("m.dark"), checked: settings.theme === "dark", action: () => ((settings.theme = "dark"), saveSettings()) },
        { label: t("m.system"), checked: settings.theme === "system", action: () => ((settings.theme = "system"), saveSettings()) },
        { sep: true },
        { label: t("m.labels"), checked: settings.labels, action: () => ((settings.labels = !settings.labels), saveSettings()) },
        { sep: true },
        { label: "English", checked: lang() === "en", action: () => ((settings.lang = "en"), saveSettings()) },
        { label: "Русский", checked: lang() === "ru", action: () => ((settings.lang = "ru"), saveSettings()) },
      ],
    },
    {
      label: t("menu.help"),
      items: [{ label: t("m.about"), action: () => (settingsOpen = true) }],
    },
  ]);

  function openContext(r: Row | null, e: MouseEvent) {
    if (!loc) return;
    const items: MenuItem[] = [];
    if (r && r.icon !== "up") items.push({ label: loc.kind === "arc" && !r.dir ? t("ctx.view") : t("ctx.open"), action: () => activate(r) });
    if (loc.kind === "disk" && selRows.length) items.push({ label: t("ctx.addTo"), action: () => cmdAdd() });
    if (can.extract) items.push({ label: t("m.extract"), action: cmdExtract });
    if (can.test) items.push({ label: t("m.test"), action: cmdTest });
    if (can.repair) items.push({ label: t("m.repair"), action: cmdRepair });
    if (can.info) items.push({ label: t("m.info"), action: cmdInfo });
    if (loc.kind === "disk" && r && r.icon !== "up" && r.icon !== "drive")
      items.push({ sep: true }, { label: t("ctx.reveal"), action: () => reveal(r.key) });
    items.push({ sep: true }, { label: t("m.refresh"), action: refresh });
    ctx = { x: Math.min(e.clientX, window.innerWidth - 240), y: Math.min(e.clientY, window.innerHeight - items.length * 32 - 20), items };
  }

  // --- Keyboard ---------------------------------------------------------------
  function key(e: KeyboardEvent) {
    if (ctx && e.key === "Escape") return (ctx = null);
    if (blocked) return;
    const inInput = (e.target as HTMLElement).tagName === "INPUT";
    const k = e.key.toLowerCase();
    const shortcuts: Record<string, () => void> = {};
    if (e.altKey) Object.assign(shortcuts, { a: () => can.add && cmdAdd(), e: () => can.extract && cmdExtract(), t: () => can.test && cmdTest(), v: () => can.view && cmdView(), i: () => can.info && cmdInfo(), r: () => can.repair && cmdRepair() });
    if (e.ctrlKey) Object.assign(shortcuts, { o: cmdOpen, f: cmdFind });
    if (shortcuts[k]) {
      e.preventDefault();
      return shortcuts[k]();
    }
    if (e.key === "F5") {
      e.preventDefault();
      return refresh();
    }
    if (inInput) {
      if (e.key === "Escape") (e.target as HTMLInputElement).blur();
      if (e.key === "ArrowDown" && e.target === findEl) findEl.blur();
      return;
    }
    if (e.key === "Backspace") {
      e.preventDefault();
      return up();
    }
    if (list?.keydown(e)) e.preventDefault();
  }

  // --- Start ------------------------------------------------------------------
  onMount(() => {
    const offs: (() => void)[] = [];
    (async () => {
      offs.push(
        await listen<ProgressEvent>("job-progress", (p) => {
          if (job && job.id === p.job && !job.done) job.progress = p;
        }),
        await listen<DoneEvent>("job-done", (d) => {
          if (!job || job.id !== d.job) return;
          job.done = d;
          // In the quick window a clean success just goes away, like WinRAR.
          if (quick && d.ok && !d.warnings.length && !d.skipped) setTimeout(() => job?.id === d.job && closeJob(), 700);
        }),
        await listen<string[]>("args", receiveArgs),
        await onFileDrop(handlePaths, (h) => (hovering = h)),
      );
      try {
        info = await api.coreInfo();
        refreshRecent();
        const args = await api.startupArgs();
        quick = args.some((a) => QUICK.includes(a));
        if (quick) receiveArgs(args);
        else if (args.length === 1 && (await api.isArchive(args[0]))) await openArchive(args[0]);
        else {
          // Start on the list of drives, like "This PC".
          await gotoDisk("");
          if (loc === null) await gotoDisk(await api.homeDir());
          receiveArgs(args);
        }
        for (const a of await api.takeForwardedArgs()) receiveArgs(a);
      } catch (e) {
        toast(String(e), "error");
      }
      // Quiet update check (only when the user set an update address).
      if (!quick && settings.autoUpdate && settings.updateUrl.trim()) {
        api
          .checkUpdate(settings.updateUrl)
          .then((u) => u && toast(t("upd.ready", { v: u.version }), "info", 9000))
          .catch(() => {});
      }
    })();
    return () => offs.forEach((f) => f());
  });

  // Keep the Explorer menu pointing at this exe and in the current language.
  $effect(() => {
    lang();
    if (info && settings.explorer && !quick) syncExplorer(true).catch(() => {});
  });

  const title = $derived(loc?.kind === "arc" ? loc.archive.name : loc?.kind === "disk" ? baseName(loc.dir.path) || t("thisPc") : "");
  const totals = $derived.by(() => {
    let files = 0, dirs = 0, size = 0;
    for (const r of rows) {
      if (r.icon === "up") continue;
      r.dir ? dirs++ : files++;
      size += r.size;
    }
    return { files, dirs, size };
  });
  const selSize = $derived(selRows.reduce((a, r) => a + r.size, 0));
</script>

<svelte:window onkeydown={key} onmousedown={(e) => ctx && !(e.target as HTMLElement).closest(".ctx") && (ctx = null)} />

<div class="app" class:quick>
  <!-- Title bar with menus -->
  <header class="titlebar" data-tauri-drag-region>
    <img class="logo" src="/logo.svg" alt="" width="20" height="20" />
    {#if !quick}<MenuBar {menus} />{/if}
    <div class="title ellipsis" data-tauri-drag-region>{title ? `${title} — Nanarchive` : "Nanarchive"}</div>
    <div class="win">
      <button onclick={win.minimize} aria-label="Minimize"><Icon name="min" size={15} /></button>
      {#if !quick}<button onclick={win.toggleMaximize} aria-label="Maximize"><Icon name="max" size={13} /></button>{/if}
      <button class="close" onclick={win.close} aria-label="Close"><Icon name="x" size={15} /></button>
    </div>
  </header>

  {#if !quick}
  <!-- Big toolbar -->
  <div class="toolbar" class:nolabels={!settings.labels}>
    <button class="tb" onclick={() => cmdAdd()} disabled={!can.add} title={can.add ? t("tip.add") : t("tip.addOff")}>
      <ToolIcon name="add" /><span>{t("tb.add")}</span>
    </button>
    <button class="tb" onclick={cmdExtract} disabled={!can.extract} title={t("tip.extract")}>
      <ToolIcon name="extract" /><span>{t("tb.extract")}</span>
    </button>
    <button class="tb" onclick={cmdTest} disabled={!can.test} title={t("tip.test")}>
      <ToolIcon name="test" /><span>{t("tb.test")}</span>
    </button>
    <button class="tb" onclick={cmdRepair} disabled={!can.repair} title={t("tip.repair")}>
      <ToolIcon name="repair" /><span>{t("tb.repair")}</span>
    </button>
    <button class="tb" onclick={cmdView} disabled={!can.view} title={t("tip.view")}>
      <ToolIcon name="view" /><span>{t("tb.view")}</span>
    </button>
    <div class="vsep"></div>
    <button class="tb" onclick={cmdOpen} title={t("tip.open")}>
      <ToolIcon name="open" /><span>{t("tb.open")}</span>
    </button>
    <button class="tb" onclick={cmdFind} title={t("tip.find")}>
      <ToolIcon name="find" /><span>{t("tb.find")}</span>
    </button>
    <button class="tb" onclick={cmdInfo} disabled={!can.info} title={t("tip.info")}>
      <ToolIcon name="info" /><span>{t("tb.info")}</span>
    </button>
    <div class="grow"></div>
    <button class="tb" onclick={() => (settingsOpen = true)} title={t("tb.settings")}>
      <ToolIcon name="settings" /><span>{t("tb.settings")}</span>
    </button>
  </div>

  <!-- Address bar -->
  <div class="addrbar">
    <button class="upbtn" onclick={up} disabled={loc?.kind === "disk" && loc.dir.parent === null} title={t("tip.up")}>
      <Icon name="up" size={17} />
    </button>
    <label class="addr">
      <FileIcon kind={loc?.kind === "arc" ? "nax" : loc?.kind === "disk" && !loc.dir.path ? "drive" : "folder"} size={17} />
      <input
        bind:value={addr}
        spellcheck="false"
        onkeydown={(e) => e.key === "Enter" && (goAddress(), (e.target as HTMLInputElement).blur())}
        onblur={() => !busy && (addr = currentAddr())}
      />
      {#if loc?.kind === "arc"}
        <span class="addr-info">{bytes(loc.archive.total_size)} → {bytes(loc.archive.archive_size)}</span>
      {/if}
    </label>
    <label class="find">
      <Icon name="search" size={15} />
      <input bind:this={findEl} bind:value={query} placeholder={loc?.kind === "arc" ? t("find.archive") : t("find.folder")} spellcheck="false" />
      {#if query}<button onclick={() => (query = "")} aria-label="Clear"><Icon name="x" size={13} /></button>{/if}
    </label>
  </div>

  <!-- File list -->
  <main class:hover={hovering}>
    <FileList
      bind:this={list}
      {rows}
      bind:selected
      onactivate={activate}
      oncontext={openContext}
      empty={debounced ? t("empty.nomatch") : t("empty.folder")}
    />
    {#if busy}<div class="loading"><span class="spin"></span></div>{/if}
    {#if hovering}
      <div class="dropveil"><div><ToolIcon name="add" size={28} /> {t("drop")}</div></div>
    {/if}
  </main>

  <!-- Status bar -->
  <footer class="status">
    <span>
      {#if selRows.length}
        {t("status.selected", { n: count(selRows.length) })}{selSize ? `, ${bytes(selSize)}` : ""}
      {:else}
        {t("status.totals", { d: count(totals.dirs), f: count(totals.files) })}{totals.size ? `, ${bytes(totals.size)}` : ""}
      {/if}
    </span>
    {#if loc?.kind === "arc"}
      {@const a = loc.archive}
      <span class="right">
        {#if a.encrypted}<span class="pill lock" title={t("info.encAes")}><Icon name="lock" size={12} /> AES-256</span>{/if}
        {#if a.recovery_percent > 0}<span class="pill shield" title={t("info.recovery")}><Icon name="shield" size={12} /> {Math.round(a.recovery_percent)}%</span>{/if}
        <span class="pill">{t("smaller", { p: a.total_size ? Math.round((1 - a.archive_size / a.total_size) * 100) : 0 })}</span>
        {t("status.files", { n: count(a.files) })} · {methodNames(a.methods)}
      </span>
    {/if}
  </footer>

  {/if}

  {#if ctx}
    <div class="ctx" style="left:{ctx.x}px; top:{ctx.y}px" role="menu">
      {#each ctx.items as it}
        {#if it.sep}<div class="sep"></div>{:else}
          <button role="menuitem" onclick={() => { ctx = null; it.action(); }}>{it.label}</button>
        {/if}
      {/each}
    </div>
  {/if}

  {#if addFor}
    <AddDialog inputs={addFor} threads={info?.threads ?? 8} onstart={startAdd} onclose={() => (addFor = null)} />
  {/if}
  {#if extractFor}
    <ExtractDialog archive={extractFor.archive} only={extractFor.only} onstart={startExtract} onclose={() => (extractFor = null)} />
  {/if}
  {#if askPw}
    <PasswordDialog name={askPw.name} wrong={askPw.wrong} onsubmit={(pw) => answerPassword(pw)} onclose={() => answerPassword(null)} />
  {/if}
  {#if infoFor}
    <InfoDialog archive={infoFor} onclose={() => (infoFor = null)} />
  {/if}
  {#if settingsOpen}
    <Settings {info} onclose={() => (settingsOpen = false)} />
  {/if}
  {#if previewing}
    <Preview
      archive={previewing.archive}
      entry={previewing.node.path}
      name={previewing.node.name}
      size={previewing.node.size}
      onclose={() => (previewing = null)}
    />
  {/if}
  {#if job}
    <Progress
      kind={job.kind}
      title={job.title}
      progress={job.progress}
      done={job.done}
      oncancel={() => job && api.cancelJob(job.id)}
      onclose={closeJob}
      onreveal={() => job?.done && reveal(job.done.output)}
      onopen={openResult}
    />
  {/if}
  <Toasts />
</div>

<style>
  .app {
    height: 100vh;
    display: flex;
    flex-direction: column;
  }

  /* Quick window: just the title bar and the dialog filling the rest. */
  .app.quick :global(.scrim) {
    top: 40px;
    background: var(--bg);
    backdrop-filter: none;
  }
  .titlebar {
    height: 40px;
    flex-shrink: 0;
    display: flex;
    align-items: center;
    gap: 6px;
    padding-left: 12px;
  }
  .logo {
    margin-right: 4px;
  }
  .title {
    flex: 1;
    align-self: stretch;
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: 12.5px;
    color: var(--muted);
    padding: 0 12px;
  }
  .win {
    display: flex;
    align-self: stretch;
  }
  .win button {
    width: 46px;
    display: grid;
    place-items: center;
    color: var(--muted);
    transition: background 0.15s, color 0.15s;
  }
  .win button:hover {
    background: var(--surface-3);
    color: var(--text);
  }
  .win .close:hover {
    background: #e5484d;
    color: white;
  }

  .toolbar {
    --tb-bg: var(--surface);
    flex-shrink: 0;
    display: flex;
    align-items: stretch;
    gap: 2px;
    margin: 0 10px;
    padding: 6px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 18px;
    box-shadow: var(--shadow-sm);
  }
  .tb {
    min-width: 72px;
    padding: 6px 8px 5px;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 3px;
    border-radius: 13px;
    font-size: 12px;
    color: var(--text);
    transition: background 0.15s, transform 0.15s var(--ease);
  }
  .tb:hover:not(:disabled) {
    background: var(--accent-softer);
  }
  .tb:active:not(:disabled) {
    transform: scale(0.95);
  }
  .tb:disabled {
    opacity: 0.38;
    filter: grayscale(0.6);
    cursor: default;
  }
  .tb :global(svg) {
    transition: transform 0.25s var(--ease);
  }
  .tb:hover:not(:disabled) :global(svg) {
    transform: translateY(-2px);
  }
  .nolabels .tb span {
    display: none;
  }
  .nolabels .tb {
    min-width: 52px;
  }
  .vsep {
    width: 1px;
    margin: 8px 6px;
    background: var(--border);
  }
  .grow {
    flex: 1;
  }

  .addrbar {
    flex-shrink: 0;
    display: flex;
    gap: 8px;
    padding: 8px 10px;
  }
  .upbtn {
    width: 36px;
    height: 36px;
    border-radius: 11px;
    display: grid;
    place-items: center;
    background: var(--surface);
    border: 1px solid var(--border);
    color: var(--muted);
  }
  .upbtn:hover:not(:disabled) {
    color: var(--accent);
    background: var(--accent-softer);
  }
  .upbtn:disabled {
    opacity: 0.4;
  }
  .addr,
  .find {
    height: 36px;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 0 10px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 11px;
    transition: border-color 0.15s, box-shadow 0.15s;
  }
  .addr:focus-within,
  .find:focus-within {
    border-color: color-mix(in srgb, var(--accent) 55%, transparent);
    box-shadow: 0 0 0 4px var(--accent-softer);
  }
  .addr {
    flex: 1;
    min-width: 0;
  }
  .addr input,
  .find input {
    flex: 1;
    min-width: 0;
    height: 100%;
    border: none;
    outline: none;
    background: none;
    font-size: 13px;
    user-select: text;
  }
  .addr-info {
    font-size: 12px;
    color: var(--muted);
    white-space: nowrap;
  }
  .find {
    width: 240px;
    color: var(--faint);
  }
  .find input {
    color: var(--text);
  }
  .find button {
    width: 22px;
    height: 22px;
    border-radius: 6px;
    display: grid;
    place-items: center;
  }
  .find button:hover {
    background: var(--surface-3);
  }

  main {
    flex: 1;
    min-height: 0;
    position: relative;
    display: flex;
    margin: 0 10px;
    border: 1px solid var(--border);
    border-radius: 16px;
    overflow: hidden;
    box-shadow: var(--shadow-sm);
    transition: border-color 0.2s;
  }
  main.hover {
    border-color: var(--accent);
  }
  .loading {
    position: absolute;
    top: 44px;
    right: 18px;
  }
  .spin {
    display: block;
    width: 16px;
    height: 16px;
    border-radius: 50%;
    border: 2.5px solid var(--accent-soft);
    border-top-color: var(--accent);
    animation: spin 0.8s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
  .dropveil {
    position: absolute;
    inset: 0;
    display: grid;
    place-items: center;
    background: color-mix(in srgb, var(--accent) 9%, transparent);
    backdrop-filter: blur(3px);
    pointer-events: none;
    animation: rise 0.2s var(--ease) both;
  }
  .dropveil div {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 12px 20px;
    border-radius: 14px;
    background: var(--surface);
    box-shadow: var(--shadow);
    font-weight: 600;
  }

  .status {
    flex-shrink: 0;
    height: 32px;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 0 18px;
    font-size: 12px;
    color: var(--muted);
  }
  .right {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .pill {
    padding: 1px 8px;
    border-radius: 99px;
    background: var(--ok-soft);
    color: var(--ok);
    font-weight: 600;
  }
  .pill.lock,
  .pill.shield {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    margin-right: 6px;
    background: var(--accent-soft);
    color: var(--accent);
  }

  .ctx {
    position: fixed;
    min-width: 220px;
    padding: 6px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 14px;
    box-shadow: var(--shadow-lg);
    z-index: 70;
    animation: rise 0.14s var(--ease) both;
  }
  .ctx button {
    width: 100%;
    height: 32px;
    padding: 0 12px;
    border-radius: 9px;
    text-align: left;
    font-size: 13px;
  }
  .ctx button:hover {
    background: var(--accent-soft);
  }
  .ctx .sep {
    height: 1px;
    margin: 5px 8px;
    background: var(--border);
  }
</style>
