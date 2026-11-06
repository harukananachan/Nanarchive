<script lang="ts">
  import Modal from "./Modal.svelte";
  import FileIcon from "./FileIcon.svelte";
  import { api, pickSavePath, PRESETS, type Estimate, type Preset } from "./api";
  import { baseName, bytes, count, duration } from "./fmt";
  import { settings } from "./state.svelte";
  import { t, type Key } from "./i18n.svelte";

  let {
    inputs,
    threads,
    onstart,
    onclose,
  }: {
    inputs: string[];
    threads: number;
    onstart: (dest: string, preset: Preset, overwrite: boolean) => void;
    onclose: () => void;
  } = $props();


  let preset = $state<Preset>(settings.preset);
  let dest = $state("");
  let overwrite = $state(false);
  let est = $state<Estimate | null>(null);

  $effect(() => {
    const list = inputs;
    api.suggestArchivePath(list).then((p) => (dest ||= p));
    api.estimate(list).then((e) => (est = e));
  });

  const idx = $derived(PRESETS.indexOf(preset));
  const size = $derived(est ? est.sizes[idx] : 0);
  const saved = $derived(est && est.bytes > 0 ? Math.max(0, 1 - size / est.bytes) : 0);
  const seconds = $derived(est ? est.seconds[idx] : 0);

  async function browse() {
    const p = await pickSavePath(dest);
    if (p) {
      dest = /\.nax$/i.test(p) ? p : `${p}.nax`;
      overwrite = true;
    }
  }

  function start() {
    settings.preset = preset;
    onstart(/\.nax$/i.test(dest) ? dest : `${dest}.nax`, preset, overwrite);
  }
</script>

<Modal title={t("add.title")} width={560} {onclose}>
  <div class="what">
    <FileIcon kind={inputs.length === 1 ? "file" : "folder"} size={22} />
    <div class="ellipsis">
      <b>{inputs.length === 1 ? baseName(inputs[0]) : t("add.items", { n: count(inputs.length) })}</b>
      <span class="muted">
        {est ? ` · ${t("add.files", { n: count(est.files) })} · ${bytes(est.bytes)}` : ` · ${t("add.counting")}`}
      </span>
    </div>
  </div>

  <label class="lab" for="arcname">{t("add.name")}</label>
  <div class="dest">
    <input id="arcname" class="field" bind:value={dest} oninput={() => (overwrite = false)} spellcheck="false" />
    <button class="btn" onclick={browse}>{t("browse")}</button>
  </div>

  <span class="lab">{t("add.compression")}</span>
  <div class="slider" role="radiogroup" aria-label="Compression preset">
    <div class="track"><div class="fill" style="width:{(idx / 4) * 100}%"></div></div>
    {#each PRESETS as p, i}
      <button
        class="stop"
        class:on={p === preset}
        class:passed={i <= idx}
        style="left:{(i / 4) * 100}%"
        role="radio"
        aria-checked={p === preset}
        onclick={() => (preset = p)}
      >
        <span class="dot"></span>
        <span class="lbl">{t(`p.${p}` as Key)}</span>
      </button>
    {/each}
  </div>
  <p class="desc">{t(`pd.${preset}` as Key)}</p>

  <div class="estimate">
    <div><span class="muted">{t("add.original")}</span><b>{est ? bytes(est.bytes) : "—"}</b></div>
    <div><span class="muted">{t("add.about")}</span><b class="acc">{est ? bytes(size) : "—"}</b></div>
    <div><span class="muted">{t("add.smallerBy")}</span><b>{est ? `${Math.round(saved * 100)}%` : "—"}</b></div>
    <div><span class="muted">{t("add.time")}</span><b>{est ? duration(seconds * 1000) : "—"}</b></div>
  </div>

  {#snippet footer()}
    <button class="btn" onclick={onclose}>{t("cancel")}</button>
    <button class="btn primary" onclick={start} disabled={!dest.trim()}>{t("add.go")}</button>
  {/snippet}
</Modal>

<style>
  .what {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 10px 12px;
    border-radius: 12px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    margin-bottom: 14px;
  }
  .lab {
    display: block;
    font-size: 12.5px;
    font-weight: 600;
    color: var(--muted);
    margin: 0 0 6px;
  }
  .dest {
    display: flex;
    gap: 8px;
    margin-bottom: 16px;
  }
  .dest .field {
    flex: 1;
    min-width: 0;
  }
  .slider {
    position: relative;
    height: 54px;
    margin: 10px 26px 0;
  }
  .track {
    position: absolute;
    left: 0;
    right: 0;
    top: 10px;
    height: 6px;
    border-radius: 99px;
    background: var(--surface-3);
    overflow: hidden;
  }
  .fill {
    height: 100%;
    background: var(--accent);
    transition: width 0.35s var(--ease);
  }
  .stop {
    position: absolute;
    top: 0;
    transform: translateX(-50%);
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 7px;
  }
  .dot {
    width: 26px;
    height: 26px;
    border-radius: 50%;
    background: var(--surface);
    border: 2px solid var(--border-strong);
    transition: transform 0.25s var(--ease), border-color 0.2s, background 0.2s, box-shadow 0.2s;
  }
  .stop.passed .dot {
    border-color: var(--accent);
  }
  .stop.on .dot {
    background: var(--accent);
    transform: scale(1.1);
    box-shadow: 0 0 0 6px var(--accent-soft);
  }
  .stop:hover .dot {
    transform: scale(1.1);
  }
  .lbl {
    font-size: 12.5px;
    font-weight: 600;
    color: var(--muted);
  }
  .stop.on .lbl {
    color: var(--accent);
  }
  .desc {
    margin: 6px 0 14px;
    min-height: 38px;
    text-align: center;
    color: var(--muted);
    font-size: 13px;
  }
  .estimate {
    display: grid;
    grid-template-columns: repeat(4, 1fr);
    gap: 8px;
  }
  .estimate div {
    display: flex;
    flex-direction: column;
    padding: 9px 12px;
    border-radius: 12px;
    background: var(--surface-2);
    border: 1px solid var(--border);
  }
  .estimate span {
    font-size: 11.5px;
  }
  .estimate b {
    font-size: 15px;
    font-variant-numeric: tabular-nums;
  }
  .acc {
    color: var(--accent);
  }
</style>
