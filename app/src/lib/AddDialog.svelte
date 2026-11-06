<script lang="ts">
  import Modal from "./Modal.svelte";
  import FileIcon from "./FileIcon.svelte";
  import Icon from "./Icon.svelte";
  import { api, pickSavePath, PRESETS, type Estimate, type Preset, type Protection } from "./api";
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
    onstart: (dest: string, preset: Preset, overwrite: boolean, protection: Protection) => void;
    onclose: () => void;
  } = $props();


  let preset = $state<Preset>(settings.preset);
  let dest = $state("");
  let overwrite = $state(false);
  let est = $state<Estimate | null>(null);

  // Protection: password (never saved), recovery record, splitting.
  let usePw = $state(false);
  let pw1 = $state("");
  let pw2 = $state("");
  let showPw = $state(false);
  let recovery = $state(settings.recovery);
  let splitMb = $state(settings.splitMb);
  const RECOVERY = [0, 1, 3, 5, 10];
  const SPLITS = [0, 100, 700, 1024, 4095];
  const pwOk = $derived(!usePw || (pw1.length > 0 && pw1 === pw2));
  const pwMismatch = $derived(usePw && pw2.length > 0 && pw1 !== pw2);
  const strength = $derived(pwStrength(pw1));

  /** 0..4, rough: length and variety of characters. */
  function pwStrength(p: string) {
    if (!p) return 0;
    let kinds = 0;
    for (const re of [/[a-z]/, /[A-Z]/, /[0-9]/, /[^a-zA-Z0-9]/]) if (re.test(p)) kinds++;
    const score = Math.min(4, Math.floor(p.length / 4) + kinds - 1);
    return Math.max(1, score);
  }

  $effect(() => {
    const list = inputs;
    api.suggestArchivePath(list).then((p) => (dest ||= p));
    api.estimate(list).then((e) => (est = e));
  });

  const idx = $derived(PRESETS.indexOf(preset));
  const size = $derived(est ? Math.round(est.sizes[idx] * (1 + recovery / 100)) : 0);
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
    if (!pwOk) return;
    settings.preset = preset;
    settings.recovery = recovery;
    settings.splitMb = splitMb;
    const protection: Protection = {
      password: usePw ? pw1 : null,
      recovery,
      volume: splitMb > 0 ? splitMb * 1024 * 1024 : null,
    };
    onstart(/\.nax$/i.test(dest) ? dest : `${dest}.nax`, preset, overwrite, protection);
  }

  const splitLabel = (mb: number) =>
    mb === 0 ? t("add.splitNone") : mb === 700 ? t("add.splitCd") : mb === 4095 ? t("add.splitFat") : bytes(mb * 1024 * 1024);
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

  <div class="prot">
    <label class="check">
      <input type="checkbox" bind:checked={usePw} />
      <Icon name="lock" size={16} />
      <span><b>{t("add.password")}</b> <span class="muted">· {t("add.passwordDesc")}</span></span>
    </label>
    {#if usePw}
      <div class="pw">
        <div class="pwfield">
          <input class="field" type={showPw ? "text" : "password"} bind:value={pw1} placeholder={t("add.pw1")} autocomplete="new-password" spellcheck="false" />
          <button class="eye" onclick={() => (showPw = !showPw)} aria-label={t("pw.show")} title={t("pw.show")}><Icon name={showPw ? "eyeoff" : "eye"} size={16} /></button>
        </div>
        <input class="field" class:bad={pwMismatch} type={showPw ? "text" : "password"} bind:value={pw2} placeholder={t("add.pw2")} autocomplete="new-password" spellcheck="false" />
      </div>
      <div class="meter" data-s={strength}><span></span><span></span><span></span><span></span></div>
      <p class="hint" class:err={pwMismatch}>{pwMismatch ? t("add.pwMismatch") : t("add.pwWarn")}</p>
    {/if}
    <div class="two">
      <label>
        <span class="lab"><Icon name="shield" size={14} /> {t("add.recovery")}</span>
        <select class="field" bind:value={recovery}>
          {#each RECOVERY as r}<option value={r}>{r === 0 ? t("add.recoveryNone") : t("add.recoveryPct", { p: r })}</option>{/each}
        </select>
      </label>
      <label>
        <span class="lab"><Icon name="archive" size={14} /> {t("add.split")}</span>
        <select class="field" bind:value={splitMb}>
          {#each SPLITS as s}<option value={s}>{splitLabel(s)}</option>{/each}
        </select>
      </label>
    </div>
    {#if recovery > 0 || splitMb > 0}
      <p class="hint">
        {recovery > 0 ? t("add.recoveryHint", { size: bytes((size * recovery) / (100 + recovery)) }) : ""}
        {splitMb > 0 ? t("add.splitHint", { name: baseName(dest).replace(/\.nax$/i, "") }) : ""}
      </p>
    {/if}
  </div>

  {#snippet footer()}
    <button class="btn" onclick={onclose}>{t("cancel")}</button>
    <button class="btn primary" onclick={start} disabled={!dest.trim() || !pwOk}>
      {#if usePw}<Icon name="lock" size={15} />{/if}
      {t("add.go")}
    </button>
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
  .prot {
    margin-top: 14px;
    padding: 12px 14px;
    border-radius: 14px;
    background: var(--surface-2);
    border: 1px solid var(--border);
  }
  .check {
    display: flex;
    align-items: center;
    gap: 8px;
    cursor: pointer;
    font-size: 13px;
  }
  .check input {
    accent-color: var(--accent);
    width: 16px;
    height: 16px;
  }
  .pw {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 8px;
    margin-top: 10px;
  }
  .pwfield {
    position: relative;
  }
  .pwfield .field {
    width: 100%;
    padding-right: 38px;
  }
  .field.bad {
    border-color: var(--err);
  }
  .eye {
    position: absolute;
    right: 5px;
    top: 50%;
    transform: translateY(-50%);
    width: 28px;
    height: 28px;
    border-radius: 8px;
    display: grid;
    place-items: center;
    color: var(--muted);
  }
  .eye:hover {
    background: var(--surface-3);
    color: var(--text);
  }
  .meter {
    display: grid;
    grid-template-columns: repeat(4, 1fr);
    gap: 4px;
    margin-top: 8px;
  }
  .meter span {
    height: 4px;
    border-radius: 99px;
    background: var(--surface-3);
    transition: background 0.2s;
  }
  .meter[data-s="1"] span:nth-child(-n + 1) {
    background: var(--err);
  }
  .meter[data-s="2"] span:nth-child(-n + 2) {
    background: #f0a04b;
  }
  .meter[data-s="3"] span:nth-child(-n + 3) {
    background: #8fc95a;
  }
  .meter[data-s="4"] span {
    background: var(--ok);
  }
  .hint {
    margin: 6px 0 0;
    font-size: 12px;
    color: var(--muted);
  }
  .hint.err {
    color: var(--err);
  }
  .two {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 10px;
    margin-top: 12px;
  }
  .two .lab {
    display: flex;
    align-items: center;
    gap: 5px;
  }
  .two select {
    width: 100%;
  }
</style>
