<script lang="ts" module>
  export type Row = {
    key: string;
    name: string;
    dir: boolean;
    size: number;
    mtime: number;
    /** Icon kind: folder, up, drive, nax, image, code, ... */
    icon: string;
    type: string;
    /** Shown after the name while searching ("in folder/sub"). */
    where?: string;
  };
</script>

<script lang="ts">
  import FileIcon from "./FileIcon.svelte";
  import { bytes, date } from "./fmt";
  import { t } from "./i18n.svelte";

  let {
    rows,
    selected = $bindable(),
    onactivate,
    oncontext,
    empty = "",
  }: {
    rows: Row[];
    selected: Set<string>;
    onactivate: (r: Row) => void;
    oncontext?: (r: Row | null, e: MouseEvent) => void;
    empty?: string;
  } = $props();

  type SortKey = "name" | "size" | "type" | "mtime";
  let sortKey = $state<SortKey>("name");
  let desc = $state(false);

  const sorted = $derived.by(() => {
    const up = rows.filter((r) => r.icon === "up");
    const rest = rows.filter((r) => r.icon !== "up");
    const d = desc ? -1 : 1;
    const by: Record<SortKey, (a: Row, b: Row) => number> = {
      name: (a, b) => a.name.localeCompare(b.name, undefined, { numeric: true, sensitivity: "base" }),
      size: (a, b) => a.size - b.size,
      type: (a, b) => a.type.localeCompare(b.type),
      mtime: (a, b) => a.mtime - b.mtime,
    };
    rest.sort((a, b) => (a.dir !== b.dir ? (a.dir ? -1 : 1) : by[sortKey](a, b) * d || by.name(a, b)));
    return [...up, ...rest];
  });

  function sortBy(k: SortKey) {
    if (sortKey === k) desc = !desc;
    else {
      sortKey = k;
      desc = k === "size" || k === "mtime";
    }
  }
  const arrow = (k: SortKey) => (sortKey === k ? (desc ? "▾" : "▴") : "");

  // Virtual list: only the visible rows exist in the page.
  const ROW = 30;
  let el: HTMLDivElement;
  let scrollTop = $state(0);
  let viewH = $state(400);
  const first = $derived(Math.max(0, Math.floor(scrollTop / ROW) - 10));
  const last = $derived(Math.min(sorted.length, Math.ceil((scrollTop + viewH) / ROW) + 10));

  let anchor = -1;
  let focus = $state(-1);

  // New listing: back to the top.
  $effect(() => {
    rows;
    focus = -1;
    anchor = -1;
    if (el) el.scrollTop = 0;
    scrollTop = 0;
  });

  function click(e: MouseEvent, i: number) {
    const r = sorted[i];
    if (e.shiftKey && anchor >= 0) {
      const [a, b] = anchor < i ? [anchor, i] : [i, anchor];
      selected = new Set(sorted.slice(a, b + 1).filter((x) => x.icon !== "up").map((x) => x.key));
    } else if (e.ctrlKey) {
      const s = new Set(selected);
      s.has(r.key) ? s.delete(r.key) : s.add(r.key);
      selected = s;
      anchor = i;
    } else {
      selected = r.icon === "up" ? new Set() : new Set([r.key]);
      anchor = i;
    }
    focus = i;
  }

  function show(i: number) {
    if (!el) return;
    const top = i * ROW;
    if (top < el.scrollTop) el.scrollTop = top;
    else if (top + ROW > el.scrollTop + viewH) el.scrollTop = top + ROW - viewH;
  }

  export function keydown(e: KeyboardEvent): boolean {
    const n = sorted.length;
    if (!n) return false;
    const step = { ArrowDown: 1, ArrowUp: -1, PageDown: Math.floor(viewH / ROW), PageUp: -Math.floor(viewH / ROW) }[e.key];
    if (step !== undefined || e.key === "Home" || e.key === "End") {
      const next = e.key === "Home" ? 0 : e.key === "End" ? n - 1 : Math.max(0, Math.min(n - 1, (focus < 0 ? -1 : focus) + step!));
      if (e.shiftKey && anchor >= 0) {
        const [a, b] = anchor < next ? [anchor, next] : [next, anchor];
        selected = new Set(sorted.slice(a, b + 1).filter((x) => x.icon !== "up").map((x) => x.key));
      } else {
        anchor = next;
        selected = sorted[next].icon === "up" ? new Set() : new Set([sorted[next].key]);
      }
      focus = next;
      show(next);
      return true;
    }
    if (e.key === "Enter" && focus >= 0) {
      onactivate(sorted[focus]);
      return true;
    }
    if (e.ctrlKey && e.key.toLowerCase() === "a") {
      selected = new Set(sorted.filter((x) => x.icon !== "up").map((x) => x.key));
      return true;
    }
    // Type a letter to jump to the next name starting with it.
    if (e.key.length === 1 && !e.ctrlKey && !e.altKey && e.key !== " ") {
      const ch = e.key.toLowerCase();
      for (let k = 1; k <= n; k++) {
        const i = (Math.max(focus, 0) + k) % n;
        if (sorted[i].name.toLowerCase().startsWith(ch)) {
          focus = anchor = i;
          selected = new Set([sorted[i].key]);
          show(i);
          return true;
        }
      }
    }
    return false;
  }
</script>

<div class="list">
  <div class="cols">
    <button onclick={() => sortBy("name")}>{t("col.name")} <i>{arrow("name")}</i></button>
    <button class="r" onclick={() => sortBy("size")}>{t("col.size")} <i>{arrow("size")}</i></button>
    <button onclick={() => sortBy("type")}>{t("col.type")} <i>{arrow("type")}</i></button>
    <button onclick={() => sortBy("mtime")}>{t("col.modified")} <i>{arrow("mtime")}</i></button>
  </div>
  <div
    class="scroll"
    bind:this={el}
    bind:clientHeight={viewH}
    onscroll={() => (scrollTop = el.scrollTop)}
    oncontextmenu={(e) => {
      if (e.target === el) {
        e.preventDefault();
        oncontext?.(null, e);
      }
    }}
    role="grid"
    tabindex="-1"
  >
    <div class="space" style="height:{sorted.length * ROW}px">
      {#each sorted.slice(first, last) as r, j (r.key)}
        {@const i = first + j}
        <div
          class="row"
          class:sel={selected.has(r.key)}
          class:focus={focus === i}
          style="transform:translateY({i * ROW}px)"
          role="row"
          tabindex="-1"
          onmousedown={(e) => e.button === 0 && click(e, i)}
          ondblclick={() => onactivate(r)}
          oncontextmenu={(e) => {
            e.preventDefault();
            if (!selected.has(r.key)) click(e, i);
            oncontext?.(r, e);
          }}
        >
          <span class="name">
            <FileIcon kind={r.icon} />
            <span class="ellipsis">{r.name}</span>
            {#if r.where}<span class="where ellipsis">in {r.where}</span>{/if}
          </span>
          <span class="r">{r.dir ? (r.size > 0 ? bytes(r.size) : "") : bytes(r.size)}</span>
          <span class="ellipsis">{r.type}</span>
          <span class="ellipsis">{date(r.mtime)}</span>
        </div>
      {/each}
    </div>
    {#if sorted.length === 0}<div class="empty">{empty}</div>{/if}
  </div>
</div>

<style>
  .list {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    background: var(--surface);
  }
  .cols,
  .row {
    display: grid;
    grid-template-columns: minmax(180px, 1fr) 100px 170px 160px;
    gap: 14px;
    align-items: center;
    padding: 0 16px;
  }
  .cols {
    height: 32px;
    flex-shrink: 0;
    border-bottom: 1px solid var(--border);
    font-size: 12px;
    font-weight: 600;
    color: var(--muted);
  }
  .cols button {
    height: 100%;
    text-align: left;
    display: flex;
    align-items: center;
    gap: 4px;
  }
  .cols button.r {
    justify-content: flex-end;
  }
  .cols button:hover {
    color: var(--text);
  }
  .cols i {
    font-style: normal;
    color: var(--accent);
    font-size: 11px;
  }
  .scroll {
    flex: 1;
    overflow: auto;
    position: relative;
    padding: 4px 6px;
    outline: none;
  }
  .space {
    position: relative;
  }
  .row {
    position: absolute;
    left: 0;
    right: 0;
    height: 30px;
    padding: 0 10px;
    border-radius: 8px;
    font-size: 13px;
    cursor: default;
    color: var(--muted);
  }
  .row:hover {
    background: var(--accent-softer);
  }
  .row.sel {
    background: var(--accent-soft);
    color: var(--text);
  }
  .row.focus {
    box-shadow: inset 0 0 0 1.5px color-mix(in srgb, var(--accent) 45%, transparent);
  }
  .name {
    display: flex;
    align-items: center;
    gap: 9px;
    min-width: 0;
    color: var(--text);
  }
  .name :global(svg) {
    flex-shrink: 0;
  }
  .where {
    font-size: 11.5px;
    color: var(--faint);
    flex-shrink: 1;
  }
  .r {
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
  .row span:not(.name) {
    font-size: 12.5px;
  }
  .empty {
    position: absolute;
    inset: 48px 0 auto;
    text-align: center;
    color: var(--muted);
  }
</style>
