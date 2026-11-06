<script lang="ts">
  // Big toolbar icons: soft pastel shapes matching the book-stack logo.
  let { name, size = 34 }: { name: string; size?: number } = $props();
</script>

<svg width={size} height={size} viewBox="0 0 40 40" aria-hidden="true">
  {#if name === "add"}
    <rect x="6" y="25" width="24" height="8" rx="3" fill="#ffb4c8" />
    <rect x="8" y="16.5" width="22" height="8" rx="3" fill="#9fe6cb" />
    <rect x="6.5" y="8" width="22.5" height="8" rx="3" fill="#aed3ff" />
    <rect x="16" y="6.5" width="4.5" height="28" rx="2" fill="#9b80fb" />
    <circle cx="31" cy="29" r="7.5" fill="#8b6cf6" stroke="var(--tb-bg, #fff)" stroke-width="2" />
    <path d="M31 25.5v7M27.5 29h7" stroke="#fff" stroke-width="2.2" stroke-linecap="round" />
  {:else if name === "extract"}
    <path d="M7 18h26v13a3 3 0 0 1-3 3H10a3 3 0 0 1-3-3z" fill="#ffd28f" />
    <path d="M5 15.5a2 2 0 0 1 2-2h26a2 2 0 0 1 2 2V19H5z" fill="#ffbf6b" />
    <rect x="15" y="23" width="10" height="3.5" rx="1.75" fill="#f3a548" opacity=".55" />
    <path d="M20 4v14" stroke="#8b6cf6" stroke-width="3.2" stroke-linecap="round" />
    <path d="M14.5 9.5 20 4l5.5 5.5" fill="none" stroke="#8b6cf6" stroke-width="3.2" stroke-linecap="round" stroke-linejoin="round" />
  {:else if name === "test"}
    <path d="M20 4.5 32.5 9v9.5c0 8-5.4 13.8-12.5 16.5C12.9 32.3 7.5 26.5 7.5 18.5V9z" fill="#9fe6cb" />
    <path d="M20 4.5V35c-7.1-2.7-12.5-8.5-12.5-16.5V9z" fill="#b8eed9" />
    <path d="m14 19.5 4.2 4.2L26.5 15" fill="none" stroke="#1f9b6f" stroke-width="3" stroke-linecap="round" stroke-linejoin="round" />
  {:else if name === "view"}
    <path d="M10 5h14l8 8v19.5a2.5 2.5 0 0 1-2.5 2.5h-19A2.5 2.5 0 0 1 8 32.5v-25A2.5 2.5 0 0 1 10 5z" fill="#aed3ff" />
    <path d="M24 5v6a2 2 0 0 0 2 2h6z" fill="#86bcf7" />
    <ellipse cx="20" cy="24" rx="8.5" ry="5.5" fill="#fff" />
    <circle cx="20" cy="24" r="3.2" fill="#8b6cf6" />
  {:else if name === "find"}
    <circle cx="17" cy="17" r="10.5" fill="#d9ebff" stroke="#7fb5f5" stroke-width="3.2" />
    <path d="M12.5 14.5a5.5 5.5 0 0 1 5-4" fill="none" stroke="#fff" stroke-width="2.4" stroke-linecap="round" />
    <path d="m25 25 8 8" stroke="#8b6cf6" stroke-width="4.4" stroke-linecap="round" />
  {:else if name === "info"}
    <circle cx="20" cy="20" r="15" fill="#c9b8ff" />
    <circle cx="20" cy="12.8" r="2.4" fill="#fff" />
    <rect x="17.8" y="17" width="4.4" height="12" rx="2.2" fill="#fff" />
  {:else if name === "settings"}
    <path
      d="M17.6 4.5h4.8l.8 4a11.5 11.5 0 0 1 3 1.7l3.8-1.4 2.4 4.2-3.1 2.6a11.6 11.6 0 0 1 0 3.5l3.1 2.6-2.4 4.2-3.8-1.4a11.5 11.5 0 0 1-3 1.7l-.8 4h-4.8l-.8-4a11.5 11.5 0 0 1-3-1.7l-3.8 1.4-2.4-4.2 3.1-2.6a11.6 11.6 0 0 1 0-3.5L7.6 13l2.4-4.2 3.8 1.4a11.5 11.5 0 0 1 3-1.7z"
      transform="translate(0 2.5)"
      fill="#ffb4c8"
    />
    <circle cx="20" cy="20" r="5" fill="#fff" />
  {:else if name === "repair"}
    <rect x="4" y="14.5" width="32" height="11" rx="5.5" transform="rotate(-45 20 20)" fill="#ffd28f" />
    <rect x="13.5" y="14.5" width="13" height="11" transform="rotate(-45 20 20)" fill="#ffb4c8" />
    <circle cx="18" cy="18" r="1.3" fill="#fff" />
    <circle cx="22" cy="18" r="1.3" fill="#fff" />
    <circle cx="18" cy="22" r="1.3" fill="#fff" />
    <circle cx="22" cy="22" r="1.3" fill="#fff" />
  {:else if name === "open"}
    <path d="M5 11a3 3 0 0 1 3-3h7l3 3h14a3 3 0 0 1 3 3v15a3 3 0 0 1-3 3H8a3 3 0 0 1-3-3z" fill="#ffd28f" />
    <path d="M5 16h30v13a3 3 0 0 1-3 3H8a3 3 0 0 1-3-3z" fill="#ffe2ab" />
  {/if}
</svg>
