<script lang="ts">
  import Modal from "./Modal.svelte";
  import FileIcon from "./FileIcon.svelte";
  import type { ArchiveView } from "./api";
  import { bytes, count } from "./fmt";
  import { t } from "./i18n.svelte";

  let { archive, onclose }: { archive: ArchiveView; onclose: () => void } = $props();
  const ratio = $derived(archive.total_size > 0 ? archive.archive_size / archive.total_size : 1);
</script>

<Modal title={t("m.info")} width={480} {onclose}>
  <div class="top">
    <FileIcon kind="nax" size={40} />
    <div class="ellipsis">
      <b>{archive.name}</b>
      <div class="muted ellipsis path" title={archive.path}>{archive.path}</div>
    </div>
  </div>

  <div class="ratio">
    <div class="bar"><div style="width:{Math.max(1, ratio * 100)}%"></div></div>
    <span class="sm">{t("smaller", { p: Math.round((1 - ratio) * 100) })}</span>
  </div>

  <dl>
    <dt>{t("info.files")}</dt><dd>{count(archive.files)}</dd>
    <dt>{t("info.folders")}</dt><dd>{count(archive.dirs)}</dd>
    <dt>{t("info.original")}</dt><dd>{bytes(archive.total_size, 2)} <span class="muted">({t("info.bytes", { n: count(archive.total_size) })})</span></dd>
    <dt>{t("info.archive")}</dt><dd>{bytes(archive.archive_size, 2)} <span class="muted">({t("info.bytes", { n: count(archive.archive_size) })})</span></dd>
    <dt>{t("info.methods")}</dt><dd>{archive.methods.join(", ")}</dd>
    {#if archive.format === "nax"}<dt>{t("info.blocks")}</dt><dd>{count(archive.blocks)}</dd>{/if}
    <dt>{t("info.format")}</dt><dd>{archive.format === "zip" ? t("info.formatZip") : t("info.formatText")}</dd>
  </dl>

  {#snippet footer()}
    <button class="btn primary" onclick={onclose}>{t("ok")}</button>
  {/snippet}
</Modal>

<style>
  .top {
    display: flex;
    align-items: center;
    gap: 12px;
    margin-bottom: 16px;
  }
  .path {
    font-size: 12px;
  }
  .ratio {
    display: flex;
    align-items: center;
    gap: 12px;
    margin-bottom: 14px;
  }
  .bar {
    flex: 1;
    height: 10px;
    border-radius: 99px;
    background: var(--surface-3);
    overflow: hidden;
  }
  .bar div {
    height: 100%;
    border-radius: 99px;
    background: var(--accent);
  }
  .ratio .sm {
    color: var(--ok);
    font-weight: 600;
  }
  dl {
    display: grid;
    grid-template-columns: 140px 1fr;
    gap: 7px 12px;
    margin: 0;
    padding: 12px 14px;
    border-radius: 14px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    font-size: 13px;
    user-select: text;
  }
  dt {
    color: var(--muted);
  }
  dd {
    margin: 0;
  }
</style>
