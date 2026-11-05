<script lang="ts">
  import Icon from "./Icon.svelte";
  import { api, type PreviewView } from "./api";
  import { bytes } from "./fmt";
  import { t } from "./i18n.svelte";
  import { onMount } from "svelte";

  let { archive, entry, name, size, onclose }: { archive: string; entry: string; name: string; size: number; onclose: () => void } =
    $props();

  let data = $state<PreviewView | null>(null);
  let error = $state("");

  onMount(async () => {
    try {
      data = await api.preview(archive, entry);
    } catch (e) {
      error = String(e);
    }
  });

  function key(e: KeyboardEvent) {
    if (e.key === "Escape" || e.key === " ") {
      e.preventDefault();
      onclose();
    }
  }
</script>

<svelte:window onkeydown={key} />

<div class="scrim" onclick={onclose} role="presentation">
  <div class="sheet card" onclick={(e) => e.stopPropagation()} role="dialog" aria-label="Preview" tabindex="-1" onkeydown={() => {}}>
    <header>
      <div class="ellipsis"><b>{name}</b> <span class="muted">· {bytes(size)}</span></div>
      <button class="btn ghost icon" onclick={onclose} title={`${t("close")} (Esc)`}><Icon name="x" /></button>
    </header>
    <div class="body">
      {#if error}
        <div class="msg err">{error}</div>
      {:else if !data}
        <div class="msg muted"><span class="spin"></span> {t("pv.unpacking")}</div>
      {:else if data.kind === "image"}
        <div class="img"><img src="data:{data.mime};base64,{data.data}" alt={name} /></div>
      {:else if data.kind === "text"}
        <pre>{data.data}{data.truncated ? `\n\n${t("pv.truncated")}` : ""}</pre>
      {:else}
        <div class="msg muted"><Icon name="eye" size={22} /> {t("pv.none")}</div>
      {/if}
    </div>
  </div>
</div>

<style>
  .scrim {
    position: fixed;
    inset: 0;
    background: var(--scrim);
    backdrop-filter: blur(6px);
    display: grid;
    place-items: center;
    z-index: 50;
    animation: fade 0.2s ease both;
  }
  .sheet {
    width: min(920px, calc(100vw - 64px));
    height: min(680px, calc(100vh - 96px));
    display: flex;
    flex-direction: column;
    box-shadow: var(--shadow-lg);
    animation: pop 0.32s var(--ease) both;
    overflow: hidden;
  }
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 10px 10px 10px 18px;
    border-bottom: 1px solid var(--border);
  }
  .body {
    flex: 1;
    min-height: 0;
    overflow: auto;
  }
  .img {
    height: 100%;
    display: grid;
    place-items: center;
    padding: 16px;
    background: var(--surface-2);
  }
  .img img {
    max-width: 100%;
    max-height: 100%;
    border-radius: 10px;
    box-shadow: var(--shadow);
  }
  pre {
    margin: 0;
    padding: 16px 20px;
    font: 12.5px/1.55 "Cascadia Code", Consolas, monospace;
    white-space: pre-wrap;
    word-break: break-word;
    user-select: text;
  }
  .msg {
    height: 100%;
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 10px;
  }
  .err {
    color: var(--err);
    padding: 20px;
  }
  .spin {
    width: 18px;
    height: 18px;
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
  @keyframes fade {
    from {
      opacity: 0;
    }
  }
  @keyframes pop {
    from {
      opacity: 0;
      transform: translateY(12px) scale(0.97);
    }
  }
</style>
