<script lang="ts" module>
  export type JobKind = "create" | "extract" | "test";
</script>

<script lang="ts">
  import Icon from "./Icon.svelte";
  import type { DoneEvent, ProgressEvent } from "./api";
  import { baseName, bytes, count, duration } from "./fmt";
  import { t, type Key } from "./i18n.svelte";

  let {
    kind,
    title,
    progress,
    done,
    oncancel,
    onclose,
    onreveal,
    onopen,
  }: {
    kind: JobKind;
    title: string;
    progress: ProgressEvent | null;
    done: DoneEvent | null;
    oncancel: () => void;
    onclose: () => void;
    onreveal: () => void;
    onopen: () => void;
  } = $props();

  // Speed from a sliding window of samples so the number stays calm.
  let samples: { t: number; b: number }[] = [];
  let speed = $state(0);
  let started = performance.now();
  $effect(() => {
    if (!progress) return;
    const now = performance.now();
    samples.push({ t: now, b: progress.bytes_done });
    while (samples.length > 2 && now - samples[0].t > 3000) samples.shift();
    const a = samples[0];
    const dt = (now - a.t) / 1000;
    if (dt > 0.25) speed = (progress.bytes_done - a.b) / dt;
  });

  const frac = $derived(
    done?.ok ? 1 : progress && progress.bytes_total > 0 ? Math.min(1, progress.bytes_done / progress.bytes_total) : 0,
  );
  const left = $derived(progress && speed > 0 ? ((progress.bytes_total - progress.bytes_done) / speed) * 1000 : NaN);
  const ratio = $derived(progress && progress.bytes_done > 0 ? progress.packed_bytes / progress.bytes_done : 0);

  const R = 74;
  const C = 2 * Math.PI * R;


  const saved = $derived(done?.ok && kind === "create" ? Math.max(0, done.bytes_in - done.bytes_out) : 0);
  const savedPct = $derived(done?.ok && kind === "create" && done.bytes_in > 0 ? saved / done.bytes_in : 0);

  function key(e: KeyboardEvent) {
    if (e.key === "Escape" && done) onclose();
  }
</script>

<svelte:window onkeydown={key} />

<div class="scrim">
  <div class="sheet card" role="dialog" aria-label={title}>
    <div class="ring" class:done={done?.ok} class:fail={done && !done.ok}>
      <svg viewBox="0 0 180 180" width="180" height="180">
        <circle cx="90" cy="90" r={R} class="bg" />
        <circle
          cx="90"
          cy="90"
          r={R}
          class="fg"
          stroke-dasharray={C}
          stroke-dashoffset={C * (1 - frac)}
          transform="rotate(-90 90 90)"
        />
      </svg>
      <div class="center">
        {#if done?.ok}
          <span class="tick"><Icon name="check" size={44} /></span>
        {:else if done}
          <span class="cross"><Icon name="x" size={40} /></span>
        {:else}
          <b>{Math.floor(frac * 100)}<small>%</small></b>
        {/if}
      </div>
      {#if done?.ok && kind === "create"}
        {#each Array(10) as _, i}
          <span class="spark" style="--a:{i * 36}deg; --d:{(i % 3) * 60}ms"></span>
        {/each}
      {/if}
    </div>

    {#if !done}
      <h2>{progress ? t(`ph.${progress.phase}` as Key) : t("pr.starting")}</h2>
      <p class="muted ellipsis sub">{title}</p>
      <div class="grid">
        <div><span class="muted">{t("pr.speed")}</span><b>{speed > 0 ? `${bytes(speed)}/s` : "—"}</b></div>
        <div><span class="muted">{t("pr.left")}</span><b>{Number.isFinite(left) && frac > 0.02 ? duration(left) : "—"}</b></div>
        {#if kind === "create"}
          <div><span class="muted">{t("pr.ratio")}</span><b>{ratio > 0 ? `${Math.round(ratio * 100)}%` : "—"}</b></div>
        {/if}
        <div><span class="muted">{t("pr.files")}</span><b>{progress ? `${count(progress.files_done)} / ${count(progress.files_total)}` : "—"}</b></div>
      </div>
      <button class="btn" onclick={oncancel}>{t("cancel")}</button>
    {:else if done.ok}
      {#if kind === "create"}
        <h2 class="big">{t("pr.saved", { size: bytes(saved) })}</h2>
        <p class="muted sub">
          {bytes(done.bytes_in)} → <b class="acc">{bytes(done.bytes_out)}</b> · {t("smaller", { p: Math.round(savedPct * 100) })} · {duration(done.elapsed_ms)}
        </p>
      {:else if kind === "extract"}
        <h2>{t("pr.extracted", { n: count(done.files) })}</h2>
        <p class="muted sub ellipsis">
          {bytes(done.bytes_out)} in {duration(done.elapsed_ms)}{done.skipped ? ` · ${t("pr.kept", { n: count(done.skipped) })}` : ""}
        </p>
      {:else}
        <h2>{t("pr.intact", { n: count(done.files) })}</h2>
        <p class="muted sub">{t("pr.checksums")} · {duration(done.elapsed_ms)}</p>
      {/if}
      {#if done.warnings.length}
        <details class="warn">
          <summary>{t("pr.skipped", { n: done.warnings.length })}</summary>
          <ul>{#each done.warnings.slice(0, 50) as w}<li>{w}</li>{/each}</ul>
        </details>
      {/if}
      <div class="row">
        {#if kind !== "test"}
          <button class="btn" onclick={onreveal}><Icon name="reveal" size={16} /> {t("pr.reveal")}</button>
        {/if}
        {#if kind === "create"}
          <button class="btn soft" onclick={onopen}><Icon name="open" size={16} /> {t("pr.open", { name: baseName(done.output) })}</button>
        {/if}
        <button class="btn primary" onclick={onclose}>{t("done")}</button>
      </div>
    {:else}
      <h2>{done.cancelled ? t("pr.cancelled") : t("pr.failed")}</h2>
      <p class="sub" class:muted={done.cancelled} class:err={!done.cancelled}>
        {done.cancelled ? t("pr.nothing") : done.error}
      </p>
      <div class="row"><button class="btn primary" onclick={onclose}>{t("close")}</button></div>
    {/if}
  </div>
</div>

<style>
  .scrim {
    position: fixed;
    inset: 0;
    background: var(--scrim);
    backdrop-filter: blur(8px);
    display: grid;
    place-items: center;
    z-index: 40;
    animation: fade 0.25s ease both;
  }
  .sheet {
    width: min(480px, calc(100vw - 48px));
    padding: 30px 30px 24px;
    text-align: center;
    box-shadow: var(--shadow-lg);
    border-radius: var(--r-xl);
    animation: pop 0.38s var(--ease) both;
  }
  .ring {
    position: relative;
    width: 180px;
    height: 180px;
    margin: 0 auto 10px;
  }
  circle {
    fill: none;
    stroke-width: 12;
  }
  .bg {
    stroke: var(--surface-3);
  }
  .fg {
    stroke: var(--accent);
    stroke-linecap: round;
    transition: stroke-dashoffset 0.35s var(--ease), stroke 0.3s;
  }
  .ring.done .fg {
    stroke: var(--ok);
  }
  .ring.fail .fg {
    stroke: var(--err);
  }
  .center {
    position: absolute;
    inset: 0;
    display: grid;
    place-items: center;
  }
  .center b {
    font-size: 40px;
    letter-spacing: -0.03em;
    font-variant-numeric: tabular-nums;
  }
  .center small {
    font-size: 20px;
    color: var(--muted);
    margin-left: 2px;
  }
  .tick {
    color: var(--ok);
    animation: pop 0.45s var(--ease) both;
  }
  .cross {
    color: var(--err);
  }
  .spark {
    position: absolute;
    left: 50%;
    top: 50%;
    width: 8px;
    height: 8px;
    margin: -4px;
    border-radius: 50%;
    background: var(--accent);
    opacity: 0;
    animation: burst 0.9s var(--ease) var(--d) both;
  }
  .spark:nth-child(3n) {
    background: #ffb4c8;
  }
  .spark:nth-child(3n + 1) {
    background: #9fe6cb;
  }
  @keyframes burst {
    0% {
      opacity: 1;
      transform: rotate(var(--a)) translateY(0) scale(0.6);
    }
    100% {
      opacity: 0;
      transform: rotate(var(--a)) translateY(-112px) scale(1);
    }
  }
  h2 {
    margin: 4px 0 2px;
    font-size: 19px;
    letter-spacing: -0.01em;
  }
  h2.big {
    font-size: 24px;
  }
  .sub {
    margin: 0 0 18px;
  }
  .acc {
    color: var(--accent);
  }
  .err {
    color: var(--err);
    word-break: break-word;
    user-select: text;
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(90px, 1fr));
    gap: 8px;
    margin-bottom: 20px;
  }
  .grid div {
    display: flex;
    flex-direction: column;
    padding: 10px 6px;
    border-radius: 12px;
    background: var(--surface-2);
    border: 1px solid var(--border);
  }
  .grid span {
    font-size: 11.5px;
  }
  .grid b {
    font-size: 14px;
    font-variant-numeric: tabular-nums;
  }
  .row {
    display: flex;
    gap: 8px;
    justify-content: center;
    flex-wrap: wrap;
  }
  .warn {
    text-align: left;
    margin: -6px 0 16px;
    font-size: 12.5px;
    color: var(--muted);
  }
  .warn ul {
    max-height: 120px;
    overflow: auto;
    margin: 6px 0 0;
    padding-left: 18px;
    user-select: text;
  }
  @keyframes fade {
    from {
      opacity: 0;
    }
  }
  @keyframes pop {
    from {
      opacity: 0;
      transform: translateY(14px) scale(0.96);
    }
  }
</style>
