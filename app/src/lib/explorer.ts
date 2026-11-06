// Explorer right-click menu and .nax icon (Windows registry, current user only).

import { api } from "./api";
import { t } from "./i18n.svelte";

export function explorerLabels() {
  return {
    file_type: t("sh.fileType"),
    open: t("sh.open"),
    extract_here: t("sh.extractHere"),
    extract_to: t("sh.extractTo"),
    test: t("sh.test"),
    menu: t("sh.menu"),
    add: t("sh.add"),
    add_quick: t("sh.addQuick"),
  };
}

/** Adds (or removes) the Explorer menu; also refreshes the exe path and labels. */
export function syncExplorer(on: boolean) {
  return on ? api.shellRegister(explorerLabels()) : api.shellUnregister();
}