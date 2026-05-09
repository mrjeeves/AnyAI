<script lang="ts">
  import type { ConversationMeta } from "../conversations";

  let { open, items, activeId, onSelect, onNew, onRename, onDelete, onClose } = $props<{
    open: boolean;
    items: ConversationMeta[];
    activeId: string | null;
    onSelect: (id: string) => void;
    onNew: () => void;
    onRename: (id: string, title: string) => void;
    onDelete: (id: string) => void;
    onClose: () => void;
  }>();

  /** Right-click menu state. Anchored to the viewport (fixed positioning),
   *  so the bounding sidebar's overflow can't clip the menu. */
  let menu = $state<{ id: string; x: number; y: number } | null>(null);
  let editingId = $state<string | null>(null);
  let editValue = $state("");

  function openMenu(e: MouseEvent, id: string) {
    e.preventDefault();
    e.stopPropagation();
    // Clamp to viewport so a row near the bottom edge doesn't push the menu
    // off-screen — measured by the menu's max dims (160 wide × ~80 tall).
    const x = Math.min(e.clientX, window.innerWidth - 170);
    const y = Math.min(e.clientY, window.innerHeight - 90);
    menu = { id, x, y };
  }

  function closeMenu() {
    menu = null;
  }

  function startRename(id: string) {
    const item = items.find((c: ConversationMeta) => c.id === id);
    if (!item) return;
    editingId = id;
    editValue = item.title;
    closeMenu();
  }

  function commitRename() {
    if (!editingId) return;
    const t = editValue.trim();
    if (t) onRename(editingId, t);
    editingId = null;
  }

  function cancelRename() {
    editingId = null;
  }

  function onRenameKey(e: KeyboardEvent) {
    if (e.key === "Enter") {
      e.preventDefault();
      commitRename();
    } else if (e.key === "Escape") {
      e.preventDefault();
      cancelRename();
    }
  }

  function deleteWithConfirm(id: string) {
    closeMenu();
    const item = items.find((c: ConversationMeta) => c.id === id);
    const label = item?.title ?? "this conversation";
    if (confirm(`Delete "${label}"? This can't be undone.`)) {
      onDelete(id);
    }
  }

  /** Group rows by recency band — same shape as ChatGPT/Claude sidebars,
   *  cheap to compute (one pass; bands are pre-sorted because `items` is). */
  function groupItems(list: ConversationMeta[]) {
    const now = Date.now();
    const day = 86400_000;
    const buckets: Array<{ label: string; rows: ConversationMeta[] }> = [
      { label: "Today", rows: [] },
      { label: "Yesterday", rows: [] },
      { label: "Previous 7 days", rows: [] },
      { label: "Older", rows: [] },
    ];
    for (const c of list) {
      const t = c.updated_at ? Date.parse(c.updated_at) : 0;
      const age = now - t;
      if (age < day) buckets[0].rows.push(c);
      else if (age < 2 * day) buckets[1].rows.push(c);
      else if (age < 7 * day) buckets[2].rows.push(c);
      else buckets[3].rows.push(c);
    }
    return buckets.filter((b) => b.rows.length > 0);
  }

  const groups = $derived(groupItems(items));
</script>

<aside class="sidebar" class:open aria-hidden={!open}>
  <div class="head">
    <button class="new" onclick={onNew} title="New chat">
      <svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true">
        <path
          fill="currentColor"
          d="M12 5a1 1 0 0 1 1 1v5h5a1 1 0 1 1 0 2h-5v5a1 1 0 1 1-2 0v-5H6a1 1 0 1 1 0-2h5V6a1 1 0 0 1 1-1z"
        />
      </svg>
      <span>New chat</span>
    </button>
    <button class="collapse" onclick={onClose} title="Hide sidebar" aria-label="Hide sidebar">
      <svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true">
        <path fill="currentColor" d="M14.7 6.3a1 1 0 0 1 0 1.4L10.4 12l4.3 4.3a1 1 0 1 1-1.4 1.4l-5-5a1 1 0 0 1 0-1.4l5-5a1 1 0 0 1 1.4 0z" />
      </svg>
    </button>
  </div>

  <div class="list" onclick={closeMenu} role="presentation">
    {#if items.length === 0}
      <div class="empty">No conversations yet.</div>
    {/if}
    {#each groups as group}
      <div class="group-label">{group.label}</div>
      {#each group.rows as c (c.id)}
        <div
          class="row"
          class:active={c.id === activeId}
          role="button"
          tabindex="0"
          onclick={() => onSelect(c.id)}
          onkeydown={(e) => {
            if (e.key === "Enter" || e.key === " ") {
              e.preventDefault();
              onSelect(c.id);
            }
          }}
          oncontextmenu={(e) => openMenu(e, c.id)}
          title={c.title}
        >
          {#if editingId === c.id}
            <input
              class="rename"
              bind:value={editValue}
              onblur={commitRename}
              onkeydown={onRenameKey}
              onclick={(e) => e.stopPropagation()}
            />
          {:else}
            <span class="title">{c.title}</span>
          {/if}
        </div>
      {/each}
    {/each}
  </div>
</aside>

{#if menu}
  <!-- Click-outside catcher: click anywhere to dismiss the context menu. -->
  <button
    class="menu-scrim"
    aria-label="Close menu"
    onclick={closeMenu}
    oncontextmenu={(e) => {
      e.preventDefault();
      closeMenu();
    }}
  ></button>
  <div class="menu" style="left: {menu.x}px; top: {menu.y}px;">
    <button onclick={() => startRename(menu!.id)}>Rename</button>
    <button class="danger" onclick={() => deleteWithConfirm(menu!.id)}>Delete</button>
  </div>
{/if}

<style>
  .sidebar {
    width: 260px;
    flex-shrink: 0;
    background: #0b0b0b;
    border-right: 1px solid #1a1a1a;
    display: flex;
    flex-direction: column;
    height: 100%;
    overflow: hidden;
    transition: margin-left .18s ease, width .18s ease;
  }
  .sidebar:not(.open) {
    margin-left: -260px;
    width: 260px;
  }
  .head {
    display: flex;
    align-items: center;
    gap: .35rem;
    padding: .45rem .5rem;
    border-bottom: 1px solid #161616;
  }
  .new {
    flex: 1;
    display: flex;
    align-items: center;
    gap: .4rem;
    background: none;
    border: 1px solid #2a2a2a;
    color: #ccc;
    padding: .35rem .55rem;
    border-radius: 7px;
    font-size: .8rem;
    cursor: pointer;
    transition: border-color .12s, color .12s, background .12s;
  }
  .new:hover { border-color: #3a3a55; color: #fff; background: #131320; }
  .collapse {
    background: none;
    border: none;
    color: #666;
    cursor: pointer;
    padding: .25rem .35rem;
    border-radius: 5px;
    display: flex;
    align-items: center;
  }
  .collapse:hover { background: #1a1a1a; color: #ccc; }
  .list {
    flex: 1;
    overflow-y: auto;
    padding: .35rem .25rem .5rem .25rem;
  }
  .empty {
    color: #555;
    font-size: .78rem;
    padding: .75rem;
    text-align: center;
  }
  .group-label {
    font-size: .68rem;
    text-transform: uppercase;
    letter-spacing: .04em;
    color: #555;
    padding: .55rem .65rem .25rem .65rem;
  }
  .row {
    display: block;
    padding: .4rem .55rem;
    margin: 1px 0;
    border-radius: 6px;
    color: #bbb;
    font-size: .82rem;
    cursor: pointer;
    user-select: none;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    transition: background .1s, color .1s;
  }
  .row:hover { background: #161616; color: #e8e8e8; }
  .row.active { background: #1c1c2e; color: #fff; }
  .row .title { display: block; overflow: hidden; text-overflow: ellipsis; }
  .rename {
    width: 100%;
    background: #1a1a1a;
    border: 1px solid #3a3a55;
    color: #fff;
    padding: .25rem .4rem;
    border-radius: 5px;
    font-size: .82rem;
    font-family: inherit;
  }
  .rename:focus { outline: none; border-color: #6e6ef7; }

  .menu-scrim {
    position: fixed;
    inset: 0;
    background: transparent;
    border: none;
    z-index: 50;
    cursor: default;
  }
  .menu {
    position: fixed;
    z-index: 51;
    background: #131320;
    border: 1px solid #2a2a3a;
    border-radius: 8px;
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.5);
    padding: .25rem;
    display: flex;
    flex-direction: column;
    min-width: 140px;
  }
  .menu button {
    text-align: left;
    background: none;
    border: none;
    color: #e8e8e8;
    font: inherit;
    font-size: .82rem;
    padding: .4rem .6rem;
    border-radius: 5px;
    cursor: pointer;
  }
  .menu button:hover { background: #1f1f33; }
  .menu button.danger { color: #ff8b8b; }
  .menu button.danger:hover { background: #2a1818; }
</style>
