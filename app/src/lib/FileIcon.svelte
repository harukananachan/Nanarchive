<script lang="ts">
  // Small filled icons for the file list.
  let { kind, size = 18 }: { kind: string; size?: number } = $props();

  const colors: Record<string, [string, string]> = {
    file: ["#dcd9e8", "#bdb8d1"],
    image: ["#ffc3d4", "#f594b2"],
    code: ["#d4c8ff", "#a993fb"],
    text: ["#c5e0ff", "#8dbdf5"],
    doc: ["#c5e0ff", "#7fb0ef"],
    archive: ["#bfeedb", "#79d3ae"],
    app: ["#dcd6ee", "#a79fc6"],
    video: ["#ffd2b5", "#f4a675"],
    audio: ["#ffd2b5", "#f4a675"],
  };
  const c = $derived(colors[kind] ?? colors.file);
</script>

<svg width={size} height={size} viewBox="0 0 20 20" aria-hidden="true">
  {#if kind === "folder" || kind === "up"}
    <path d="M1.5 5a2 2 0 0 1 2-2h4l2 2h7a2 2 0 0 1 2 2v8.5a2 2 0 0 1-2 2h-13a2 2 0 0 1-2-2z" fill="#ffcf6e" />
    <path d="M1.5 7.5h17v8a2 2 0 0 1-2 2h-13a2 2 0 0 1-2-2z" fill="#ffe09a" />
    {#if kind === "up"}<path d="M10 15.5v-6M7.5 11.5 10 9l2.5 2.5" fill="none" stroke="#c98f1c" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" />{/if}
  {:else if kind === "drive"}
    <rect x="2" y="5" width="16" height="10" rx="3" fill="#d4dcef" />
    <rect x="2" y="10.5" width="16" height="4.5" rx="2.2" fill="#b7c2dd" />
    <circle cx="14.5" cy="12.8" r="1.1" fill="#3fc28d" />
  {:else if kind === "nax"}
    <rect x="3" y="12.5" width="14" height="4.5" rx="1.8" fill="#ffb4c8" />
    <rect x="4" y="8" width="13" height="4.5" rx="1.8" fill="#9fe6cb" />
    <rect x="3.3" y="3.5" width="13.4" height="4.5" rx="1.8" fill="#aed3ff" />
    <rect x="8.6" y="2.8" width="2.8" height="15" rx="1.2" fill="#8b6cf6" />
  {:else}
    <path d="M5 1.5h6.5L16 6v11a1.5 1.5 0 0 1-1.5 1.5h-9.5A1.5 1.5 0 0 1 3.5 17V3A1.5 1.5 0 0 1 5 1.5z" fill={c[0]} />
    <path d="M11.5 1.5V5a1 1 0 0 0 1 1H16z" fill={c[1]} />
    <rect x="6" y="10" width="6" height="1.6" rx=".8" fill={c[1]} />
    <rect x="6" y="13" width="4" height="1.6" rx=".8" fill={c[1]} />
  {/if}
</svg>
