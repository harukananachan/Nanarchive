<script lang="ts">
  import Icon from "./Icon.svelte";
  import { dismiss, toasts } from "./state.svelte";
</script>

<div class="stack" aria-live="polite">
  {#each toasts as t (t.id)}
    <div class="toast {t.kind}">
      <Icon name={t.kind === "error" ? "x" : t.kind === "ok" ? "check" : "sparkle"} size={16} />
      <span>{t.text}</span>
      <button onclick={() => dismiss(t.id)} aria-label="Dismiss"><Icon name="x" size={13} /></button>
    </div>
  {/each}
</div>

<style>
  .stack {
    position: fixed;
    right: 18px;
    bottom: 18px;
    display: flex;
    flex-direction: column;
    gap: 8px;
    z-index: 60;
    max-width: min(420px, calc(100vw - 36px));
  }
  .toast {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 10px 10px 10px 14px;
    border-radius: 14px;
    background: var(--surface);
    border: 1px solid var(--border);
    box-shadow: var(--shadow);
    animation: slide 0.35s var(--ease) both;
    user-select: text;
  }
  .toast span {
    flex: 1;
    word-break: break-word;
  }
  .toast.error {
    color: var(--err);
  }
  .toast.ok {
    color: var(--ok);
  }
  .toast span {
    color: var(--text);
  }
  button {
    width: 24px;
    height: 24px;
    border-radius: 7px;
    display: grid;
    place-items: center;
    color: var(--faint);
  }
  button:hover {
    background: var(--surface-3);
  }
  @keyframes slide {
    from {
      opacity: 0;
      transform: translateY(10px);
    }
  }
</style>
