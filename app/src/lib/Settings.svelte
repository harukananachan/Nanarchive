<script lang="ts">
  import Modal from "./Modal.svelte";
  import { PRESETS, type CoreInfo } from "./api";
  import { ACCENTS, saveSettings, settings, type Accent, type LangSetting, type Theme } from "./state.svelte";
  import { t, type Key } from "./i18n.svelte";
  import { syncExplorer } from "./explorer";
  import { toast } from "./state.svelte";
  import { api, listen, type UpdateInfo } from "./api";
  import { onDestroy } from "svelte";

  // --- Updates ---
  let checking = $state(false);
  let found = $state<UpdateInfo | null>(null);
  let upState = $state("");
  let installing = $state(false);
  let got = $state(0);
  let total = $state(0);
  let unlisten: (() => void) | null = null;
  onDestroy(() => unlisten?.());

  function upError(e: unknown) {
    const s = String(e);
    return s === "bad_url" ? t("upd.badUrl") : t("upd.failed", { e: s });
  }

  async function checkNow() {
    if (!settings.updateUrl.trim()) return;
    checking = true;
    found = null;
    upState = "";
    try {
      found = await api.checkUpdate(settings.updateUrl);
      upState = found ? t("upd.found", { v: found.version }) : t("upd.latest");
    } catch (e) {
      upState = upError(e);
    } finally {
      checking = false;
    }
  }

  async function install() {
    installing = true;
    got = 0;
    total = 0;
    unlisten ??= await listen<{ done: number; total: number }>("update-progress", (p) => {
      got = p.done;
      total = p.total;
    });
    try {
      await api.installUpdate(settings.updateUrl);
    } catch (e) {
      installing = false;
      upState = upError(e);
    }
  }

  async function toggleExplorer() {
    const on = !settings.explorer;
    try {
      await syncExplorer(on);
      settings.explorer = on;
      toast(on ? t("set.explorerOn") : t("set.explorerOff"), "ok");
    } catch (e) {
      toast(String(e), "error", 6000);
    }
  }

  let { info, onclose }: { info: CoreInfo | null; onclose: () => void } = $props();

  const themes: { id: Theme; key: Key }[] = [
    { id: "system", key: "set.system" },
    { id: "light", key: "set.light" },
    { id: "dark", key: "set.dark" },
  ];
  const langs: { id: LangSetting; label: () => string }[] = [
    { id: "auto", label: () => t("set.langAuto") },
    { id: "en", label: () => "English" },
    { id: "ru", label: () => "Русский" },
  ];

  $effect(() => {
    JSON.stringify(settings);
    saveSettings();
  });
</script>

<Modal title={t("set.title")} width={580} {onclose}>
  <div class="item">
    <div><b>{t("set.lang")}</b><p class="muted">{t("set.langDesc")}</p></div>
    <div class="seg">
      {#each langs as l}
        <button class:on={settings.lang === l.id} onclick={() => (settings.lang = l.id)}>{l.label()}</button>
      {/each}
    </div>
  </div>
  <div class="item">
    <div><b>{t("set.theme")}</b><p class="muted">{t("set.themeDesc")}</p></div>
    <div class="seg">
      {#each themes as th}
        <button class:on={settings.theme === th.id} onclick={() => (settings.theme = th.id)}>{t(th.key)}</button>
      {/each}
    </div>
  </div>
  <div class="item">
    <div><b>{t("set.accent")}</b><p class="muted">{t("set.accentDesc")}</p></div>
    <div class="swatches">
      {#each Object.entries(ACCENTS) as [id, a]}
        <button
          class="sw"
          class:on={settings.accent === id}
          style="--c:{a.color}"
          title={t(`accent.${id}` as Key)}
          aria-label={t(`accent.${id}` as Key)}
          onclick={() => (settings.accent = id as Accent)}
        ></button>
      {/each}
    </div>
  </div>
  <div class="item">
    <div><b>{t("set.labels")}</b><p class="muted">{t("set.labelsDesc")}</p></div>
    <button class="switch" class:on={settings.labels} onclick={() => (settings.labels = !settings.labels)} aria-label={t("set.labels")}>
      <span></span>
    </button>
  </div>
  <div class="item">
    <div><b>{t("set.motion")}</b><p class="muted">{t("set.motionDesc")}</p></div>
    <button class="switch" class:on={settings.motion} onclick={() => (settings.motion = !settings.motion)} aria-label={t("set.motion")}>
      <span></span>
    </button>
  </div>
  <div class="item">
    <div><b>{t("set.explorer")}</b><p class="muted">{t("set.explorerDesc")}</p></div>
    <button class="switch" class:on={settings.explorer} onclick={toggleExplorer} aria-label={t("set.explorer")}>
      <span></span>
    </button>
  </div>
  <div class="item">
    <div><b>{t("set.preset")}</b><p class="muted">{t("set.presetDesc")}</p></div>
    <select class="field" bind:value={settings.preset}>
      {#each PRESETS as p}<option value={p}>{t(`p.${p}` as Key)}</option>{/each}
    </select>
  </div>

  <div class="item col">
    <div><b>{t("upd.title")}</b><p class="muted">{t("upd.desc")}</p></div>
    <div class="row">
      <input class="field grow" placeholder="https://example.com/nanarchive/latest.json" bind:value={settings.updateUrl} spellcheck="false" />
      <button class="btn" disabled={checking || installing || !settings.updateUrl.trim()} onclick={checkNow}>{checking ? t("upd.checking") : t("upd.check")}</button>
    </div>
    <label class="check"><input type="checkbox" bind:checked={settings.autoUpdate} /> {t("upd.auto")}</label>
    {#if installing}
      <p class="muted">{t("upd.downloading")} {total ? Math.round((got / total) * 100) + "%" : ""}</p>
    {:else if upState}
      <p class="muted">{upState}</p>
    {/if}
    {#if found && !installing}
      {#if found.notes}<p class="notes">{found.notes}</p>{/if}
      <div><button class="btn primary" onclick={install}>{t("upd.install", { v: found.version })}</button></div>
    {/if}
  </div>

  <div class="about">
    <img src="/logo.svg" alt="" width="40" height="40" />
    <div>
      <b>Nanarchive {info?.version ?? ""}</b>
      <p class="muted">{t("set.about", { v: info?.format_version ?? 1, n: info?.threads ?? "?" })}</p>
    </div>
  </div>

  {#snippet footer()}
    <button class="btn primary" onclick={onclose}>{t("done")}</button>
  {/snippet}
</Modal>

<style>
  .item {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 20px;
    padding: 12px 0;
  }
  .item + .item {
    border-top: 1px solid var(--border);
  }
  .item p {
    margin: 2px 0 0;
    font-size: 12.5px;
  }
  .seg {
    display: flex;
    flex-shrink: 0;
    padding: 3px;
    border-radius: 12px;
    background: var(--surface-3);
  }
  .seg button {
    height: 30px;
    padding: 0 12px;
    border-radius: 9px;
    font-weight: 600;
    font-size: 13px;
    color: var(--muted);
    transition: background 0.2s, color 0.2s;
  }
  .seg button.on {
    background: var(--surface);
    color: var(--text);
    box-shadow: var(--shadow-sm);
  }
  .swatches {
    display: flex;
    gap: 10px;
  }
  .sw {
    width: 26px;
    height: 26px;
    border-radius: 50%;
    background: var(--c);
    transition: transform 0.2s var(--ease), box-shadow 0.2s;
  }
  .sw:hover {
    transform: scale(1.1);
  }
  .sw.on {
    box-shadow: 0 0 0 3px var(--surface), 0 0 0 5px var(--c);
  }
  .switch {
    width: 46px;
    height: 26px;
    border-radius: 99px;
    background: var(--surface-3);
    position: relative;
    transition: background 0.2s;
    flex-shrink: 0;
  }
  .switch span {
    position: absolute;
    top: 3px;
    left: 3px;
    width: 20px;
    height: 20px;
    border-radius: 50%;
    background: white;
    box-shadow: 0 1px 3px rgba(0, 0, 0, 0.25);
    transition: transform 0.25s var(--ease);
  }
  .switch.on {
    background: var(--accent);
  }
  .switch.on span {
    transform: translateX(20px);
  }
  .item.col {
    flex-direction: column;
    align-items: stretch;
    gap: 8px;
  }
  .row {
    display: flex;
    gap: 8px;
  }
  .grow {
    flex: 1;
    min-width: 0;
  }
  .check {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 13px;
  }
  .notes {
    margin: 0;
    font-size: 12.5px;
    white-space: pre-wrap;
    max-height: 90px;
    overflow: auto;
  }
  select.field {
    min-width: 150px;
  }
  .about {
    display: flex;
    align-items: center;
    gap: 12px;
    margin-top: 8px;
    padding: 12px 14px;
    border-radius: 14px;
    background: var(--surface-2);
    border: 1px solid var(--border);
  }
  .about p {
    margin: 2px 0 0;
    font-size: 12px;
  }
</style>
