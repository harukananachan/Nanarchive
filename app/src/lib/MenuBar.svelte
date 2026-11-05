<script lang="ts" module>
  export type MenuItem =
    | { sep: true }
    | { label: string; shortcut?: string; action: () => void; disabled?: boolean; checked?: boolean; sep?: false };
  export type Menu = { label: string; items: MenuItem[] };
</script>

<script lang="ts">
  import Icon from "./Icon.svelte";

  let { menus }: { menus: Menu[] } = $props();
  let open = $state(-1);

  function run(item: MenuItem) {
    if (item.sep || item.disabled) return;
    open = -1;
    item.action();
  }
</script>

<svelte:window
  onmousedown={(e) => {
    if (open >= 0 && !(e.target as HTMLElement).closest(".menubar")) open = -1;
  }}
  onkeydown={(e) => e.key === "Escape" && (open = -1)}
  onblur={() => (open = -1)}
/>

<nav class="menubar">
  {#each menus as m, i}
    <div class="m">
      <button
        class="top"
        class:on={open === i}
        onmousedown={() => (open = open === i ? -1 : i)}
        onmouseenter={() => open >= 0 && (open = i)}
      >
        {m.label}
      </button>
      {#if open === i}
        <div class="drop" role="menu">
          {#each m.items as it}
            {#if it.sep}
              <div class="sep"></div>
            {:else}
              <button class="item" disabled={it.disabled} role="menuitem" onclick={() => run(it)}>
                <span class="chk">{#if it.checked}<Icon name="check" size={14} />{/if}</span>
                <span class="lbl ellipsis">{it.label}</span>
                {#if it.shortcut}<span class="sc">{it.shortcut}</span>{/if}
              </button>
            {/if}
          {/each}
        </div>
      {/if}
    </div>
  {/each}
</nav>

<style>
  .menubar {
    display: flex;
    gap: 2px;
  }
  .m {
    position: relative;
  }
  .top {
    height: 30px;
    padding: 0 11px;
    border-radius: 9px;
    font-size: 13px;
    color: var(--text);
    transition: background 0.12s;
  }
  .top:hover,
  .top.on {
    background: var(--accent-soft);
  }
  .drop {
    position: absolute;
    top: 34px;
    left: 0;
    min-width: 250px;
    max-width: 420px;
    padding: 6px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 14px;
    box-shadow: var(--shadow-lg);
    z-index: 70;
    animation: drop 0.16s var(--ease) both;
  }
  .item {
    width: 100%;
    height: 32px;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 0 10px 0 6px;
    border-radius: 9px;
    font-size: 13px;
    text-align: left;
  }
  .item:hover:not(:disabled) {
    background: var(--accent-soft);
  }
  .item:disabled {
    color: var(--faint);
    cursor: default;
  }
  .chk {
    width: 18px;
    display: grid;
    place-items: center;
    color: var(--accent);
  }
  .lbl {
    flex: 1;
  }
  .sc {
    font-size: 11.5px;
    color: var(--faint);
  }
  .sep {
    height: 1px;
    margin: 5px 8px;
    background: var(--border);
  }
  @keyframes drop {
    from {
      opacity: 0;
      transform: translateY(-4px);
    }
  }
</style>
