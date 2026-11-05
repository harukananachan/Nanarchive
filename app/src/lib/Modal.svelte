<script lang="ts">
  import type { Snippet } from "svelte";
  import Icon from "./Icon.svelte";

  let {
    title,
    width = 520,
    onclose,
    children,
    footer,
  }: { title: string; width?: number; onclose: () => void; children: Snippet; footer?: Snippet } = $props();

  function key(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.stopPropagation();
      onclose();
    }
  }
</script>

<svelte:window onkeydown={key} />

<div class="scrim" onmousedown={(e) => e.target === e.currentTarget && onclose()} role="presentation">
  <div class="dlg" style="width:min({width}px, calc(100vw - 40px))" role="dialog" aria-label={title}>
    <header>
      <h2>{title}</h2>
      <button class="x" onclick={onclose} aria-label="Close"><Icon name="x" size={15} /></button>
    </header>
    <div class="body">{@render children()}</div>
    {#if footer}<footer>{@render footer()}</footer>{/if}
  </div>
</div>

<style>
  .scrim {
    position: fixed;
    inset: 0;
    background: var(--scrim);
    backdrop-filter: blur(5px);
    display: grid;
    place-items: center;
    z-index: 45;
    animation: fade 0.18s ease both;
  }
  .dlg {
    max-height: calc(100vh - 40px);
    display: flex;
    flex-direction: column;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--r-xl);
    box-shadow: var(--shadow-lg);
    animation: pop 0.3s var(--ease) both;
  }
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 16px 14px 6px 22px;
  }
  h2 {
    margin: 0;
    font-size: 16px;
    letter-spacing: -0.01em;
  }
  .x {
    width: 30px;
    height: 30px;
    border-radius: 9px;
    display: grid;
    place-items: center;
    color: var(--muted);
  }
  .x:hover {
    background: var(--surface-3);
    color: var(--text);
  }
  .body {
    padding: 10px 22px 6px;
    overflow: auto;
  }
  footer {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    padding: 14px 22px 20px;
  }
  @keyframes fade {
    from {
      opacity: 0;
    }
  }
  @keyframes pop {
    from {
      opacity: 0;
      transform: translateY(10px) scale(0.97);
    }
  }
</style>
