<script lang="ts">
  import Modal from "./Modal.svelte";
  import { api, pickFolder } from "./api";
  import { baseName, count } from "./fmt";
  import { t } from "./i18n.svelte";

  let {
    archive,
    only,
    onstart,
    onclose,
  }: {
    archive: string;
    only: string[];
    onstart: (dest: string, overwrite: boolean) => void;
    onclose: () => void;
  } = $props();

  let dest = $state("");
  let overwrite = $state(false);

  $effect(() => {
    api.suggestExtractDir(archive).then((d) => (dest ||= d));
  });

  async function browse() {
    const p = await pickFolder(t("ex.dest"));
    if (p) dest = p;
  }
</script>

<Modal title={t("ex.title")} width={540} {onclose}>
  <p class="what">
    <b>{only.length ? t("ex.some", { n: count(only.length) }) : t("ex.all")}</b>
    <span class="muted"> {t("ex.from", { name: baseName(archive) })}</span>
  </p>

  <label class="lab" for="dest">{t("ex.dest")}</label>
  <div class="dest">
    <input id="dest" class="field" bind:value={dest} spellcheck="false" />
    <button class="btn" onclick={browse}>{t("browse")}</button>
  </div>
  <p class="hint muted">{t("ex.hint")}</p>

  <span class="lab">{t("ex.exists")}</span>
  <div class="seg">
    <button class:on={!overwrite} onclick={() => (overwrite = false)}>{t("ex.keep")}</button>
    <button class:on={overwrite} onclick={() => (overwrite = true)}>{t("ex.replace")}</button>
  </div>

  {#snippet footer()}
    <button class="btn" onclick={onclose}>{t("cancel")}</button>
    <button class="btn primary" onclick={() => onstart(dest.trim(), overwrite)} disabled={!dest.trim()}>{t("ex.go")}</button>
  {/snippet}
</Modal>

<style>
  .what {
    margin: 0 0 14px;
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
  }
  .dest .field {
    flex: 1;
    min-width: 0;
  }
  .hint {
    font-size: 12px;
    margin: 6px 0 16px;
  }
  .seg {
    display: inline-flex;
    padding: 3px;
    border-radius: 12px;
    background: var(--surface-3);
  }
  .seg button {
    height: 32px;
    padding: 0 14px;
    border-radius: 9px;
    font-size: 13px;
    font-weight: 600;
    color: var(--muted);
    transition: background 0.2s, color 0.2s;
  }
  .seg button.on {
    background: var(--surface);
    color: var(--text);
    box-shadow: var(--shadow-sm);
  }
</style>
