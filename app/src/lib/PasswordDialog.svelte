<script lang="ts">
  import Modal from "./Modal.svelte";
  import Icon from "./Icon.svelte";
  import { t } from "./i18n.svelte";

  let {
    name,
    wrong,
    onsubmit,
    onclose,
  }: { name: string; wrong: boolean; onsubmit: (pw: string) => void; onclose: () => void } = $props();

  let pw = $state("");
  let show = $state(false);
  let input = $state<HTMLInputElement>();
  $effect(() => input?.focus());

  function go() {
    if (pw) onsubmit(pw);
  }
</script>

<Modal title={t("pw.title")} width={440} {onclose}>
  <div class="head">
    <span class="lock"><Icon name="lock" size={22} /></span>
    <p>{t("pw.ask", { name })}</p>
  </div>
  <div class="row">
    <input
      bind:this={input}
      class="field"
      class:bad={wrong}
      type={show ? "text" : "password"}
      bind:value={pw}
      placeholder={t("pw.placeholder")}
      autocomplete="off"
      spellcheck="false"
      onkeydown={(e) => e.key === "Enter" && go()}
    />
    <button class="eye" onclick={() => (show = !show)} aria-label={t("pw.show")} title={t("pw.show")}>
      <Icon name={show ? "eyeoff" : "eye"} size={17} />
    </button>
  </div>
  {#if wrong}<p class="err">{t("pw.wrong")}</p>{/if}

  {#snippet footer()}
    <button class="btn" onclick={onclose}>{t("cancel")}</button>
    <button class="btn primary" onclick={go} disabled={!pw}>{t("pw.unlock")}</button>
  {/snippet}
</Modal>

<style>
  .head {
    display: flex;
    align-items: center;
    gap: 12px;
    margin-bottom: 14px;
  }
  .head p {
    margin: 0;
    word-break: break-word;
  }
  .lock {
    flex-shrink: 0;
    width: 42px;
    height: 42px;
    border-radius: 14px;
    display: grid;
    place-items: center;
    color: var(--accent);
    background: var(--accent-soft);
  }
  .row {
    position: relative;
  }
  .row .field {
    width: 100%;
    padding-right: 42px;
  }
  .field.bad {
    border-color: var(--err);
  }
  .eye {
    position: absolute;
    right: 6px;
    top: 50%;
    transform: translateY(-50%);
    width: 30px;
    height: 30px;
    border-radius: 9px;
    display: grid;
    place-items: center;
    color: var(--muted);
  }
  .eye:hover {
    background: var(--surface-3);
    color: var(--text);
  }
  .err {
    color: var(--err);
    margin: 8px 0 0;
    font-size: 13px;
  }
</style>