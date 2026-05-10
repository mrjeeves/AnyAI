<script lang="ts">
  import type { ConversationMeta, FolderMeta } from "../conversations";
  import type { Mode } from "../types";

  let {
    open,
    items,
    folders,
    activeId,
    mode,
    onSelect,
    onNew,
    onRename,
    onDelete,
    onMove,
    onCreateFolder,
    onRenameFolder,
    onDeleteFolder,
    onClose,
  } = $props<{
    open: boolean;
    items: ConversationMeta[];
    folders: FolderMeta[];
    activeId: string | null;
    /** Active mode. Drives whether we say "chat" or "session" in the
     *  sidebar copy — same list, different metaphor. */
    mode: Mode;
    onSelect: (id: string) => void;
    onNew: () => void;
    onRename: (id: string, title: string) => void;
    onDelete: (id: string) => void;
    /** Move a conversation file into the given folder path (POSIX, "" for root). */
    onMove: (id: string, folder: string) => void;
    onCreateFolder: (path: string) => void;
    onRenameFolder: (oldPath: string, newPath: string) => void;
    onDeleteFolder: (path: string) => void;
    onClose: () => void;
  }>();

  const newLabel = $derived(mode === "transcribe" ? "New session" : "New chat");
  const emptyLabel = $derived(
    mode === "transcribe" ? "No sessions yet." : "No conversations yet.",
  );
  const itemNoun = $derived(mode === "transcribe" ? "session" : "conversation");

  /** Right-click menu state. Anchored to the viewport (fixed positioning),
   *  so the bounding sidebar's overflow can't clip the menu. */
  type MenuTarget =
    | { kind: "item"; id: string }
    | { kind: "folder"; path: string };
  let menu = $state<{ target: MenuTarget; x: number; y: number } | null>(null);
  let editingId = $state<string | null>(null);
  let editingFolder = $state<string | null>(null);
  let editValue = $state("");

  /** Folder paths the user has collapsed. Folders default to expanded. */
  let collapsed = $state<Set<string>>(new Set());

  /** Drop target highlighting. We track the path the user is currently
   *  hovering with a drag so the row paints a border / background and the
   *  user knows where the file will land. "" === root. */
  let dragOverPath = $state<string | null>(null);

  function openItemMenu(e: MouseEvent, id: string) {
    e.preventDefault();
    e.stopPropagation();
    const x = Math.min(e.clientX, window.innerWidth - 170);
    const y = Math.min(e.clientY, window.innerHeight - 130);
    menu = { target: { kind: "item", id }, x, y };
  }

  function openFolderMenu(e: MouseEvent, path: string) {
    e.preventDefault();
    e.stopPropagation();
    const x = Math.min(e.clientX, window.innerWidth - 200);
    const y = Math.min(e.clientY, window.innerHeight - 170);
    menu = { target: { kind: "folder", path }, x, y };
  }

  function closeMenu() {
    menu = null;
  }

  function startRenameItem(id: string) {
    const item = items.find((c: ConversationMeta) => c.id === id);
    if (!item) return;
    editingId = id;
    editValue = item.title;
    closeMenu();
  }

  function commitRenameItem() {
    if (!editingId) return;
    const t = editValue.trim();
    if (t) onRename(editingId, t);
    editingId = null;
  }

  function startRenameFolder(path: string) {
    editingFolder = path;
    editValue = path.split("/").pop() ?? "";
    closeMenu();
  }

  function commitRenameFolder() {
    if (!editingFolder) return;
    const trimmed = editValue.trim();
    if (trimmed) {
      const parts = editingFolder.split("/");
      parts[parts.length - 1] = trimmed;
      const next = parts.join("/");
      if (next !== editingFolder) onRenameFolder(editingFolder, next);
    }
    editingFolder = null;
  }

  function cancelRename() {
    editingId = null;
    editingFolder = null;
  }

  function onRenameKey(e: KeyboardEvent, kind: "item" | "folder") {
    if (e.key === "Enter") {
      e.preventDefault();
      if (kind === "item") commitRenameItem();
      else commitRenameFolder();
    } else if (e.key === "Escape") {
      e.preventDefault();
      cancelRename();
    }
  }

  function deleteItemWithConfirm(id: string) {
    closeMenu();
    const item = items.find((c: ConversationMeta) => c.id === id);
    const label = item?.title ?? `this ${itemNoun}`;
    if (confirm(`Delete "${label}"? This can't be undone.`)) {
      onDelete(id);
    }
  }

  function deleteFolderWithConfirm(path: string) {
    closeMenu();
    const childCount = items.filter(
      (c: ConversationMeta) => c.path === path || c.path.startsWith(path + "/"),
    ).length;
    const childFolderCount = folders.filter(
      (f: FolderMeta) => f.path !== path && f.path.startsWith(path + "/"),
    ).length;
    const total = childCount + childFolderCount;
    let prompt = `Delete folder "${path.split("/").pop()}"?`;
    if (total > 0) {
      prompt += ` This also removes ${childCount} ${itemNoun}${childCount === 1 ? "" : "s"}`;
      if (childFolderCount > 0) {
        prompt += ` and ${childFolderCount} subfolder${childFolderCount === 1 ? "" : "s"}`;
      }
      prompt += " inside it.";
    }
    prompt += " This can't be undone.";
    if (confirm(prompt)) onDeleteFolder(path);
  }

  function promptCreateFolder(parent: string) {
    closeMenu();
    const name = window.prompt(
      parent
        ? `New folder name (inside "${parent}"):`
        : "New folder name:",
    );
    if (!name) return;
    const trimmed = name.trim();
    if (!trimmed) return;
    onCreateFolder(parent ? `${parent}/${trimmed}` : trimmed);
  }

  function moveToRoot(id: string) {
    closeMenu();
    onMove(id, "");
  }

  /** Build the visible tree. Each folder appears once with its direct-child
   *  conversations grouped underneath. Walks the sorted folder list in
   *  document order, and uses the conversation `path` field to slot rows
   *  into the right node. */
  type Node = {
    path: string;
    name: string;
    depth: number;
    children: Node[];
    items: ConversationMeta[];
  };

  const tree = $derived.by((): Node => {
    const root: Node = { path: "", name: "", depth: 0, children: [], items: [] };
    const byPath = new Map<string, Node>();
    byPath.set("", root);
    // Build folder skeleton first so empty folders still render.
    const allFolderPaths = new Set<string>();
    for (const f of folders) allFolderPaths.add(f.path);
    // Materialise any folder paths referenced by items but somehow missing
    // from the folders list (defensive — listConversations should already
    // surface them).
    for (const it of items) if (it.path) allFolderPaths.add(it.path);
    const sortedFolders = [...allFolderPaths].sort();
    for (const path of sortedFolders) {
      const parts = path.split("/");
      // Walk ancestors so an entry like "A/B/C" creates A, A/B, A/B/C.
      for (let i = 1; i <= parts.length; i++) {
        const sub = parts.slice(0, i).join("/");
        if (byPath.has(sub)) continue;
        const parent = byPath.get(parts.slice(0, i - 1).join("/")) ?? root;
        const node: Node = {
          path: sub,
          name: parts[i - 1],
          depth: i - 1,
          children: [],
          items: [],
        };
        parent.children.push(node);
        byPath.set(sub, node);
      }
    }
    // Distribute items into their folders.
    for (const it of items) {
      const node = byPath.get(it.path) ?? root;
      node.items.push(it);
    }
    return root;
  });

  /** Group a list into ChatGPT-style time bands. Pure helper, no derived. */
  function groupByBand(rows: ConversationMeta[]) {
    const now = Date.now();
    const day = 86400_000;
    const buckets = [
      { label: "Today", rows: [] as ConversationMeta[] },
      { label: "Yesterday", rows: [] as ConversationMeta[] },
      { label: "Previous 7 days", rows: [] as ConversationMeta[] },
      { label: "Older", rows: [] as ConversationMeta[] },
    ];
    for (const r of rows) {
      const t = r.updated_at ? Date.parse(r.updated_at) : 0;
      const age = now - t;
      if (age < day) buckets[0].rows.push(r);
      else if (age < 2 * day) buckets[1].rows.push(r);
      else if (age < 7 * day) buckets[2].rows.push(r);
      else buckets[3].rows.push(r);
    }
    return buckets.filter((b) => b.rows.length > 0);
  }

  // ---------------------------------------------------------------------
  // Drag-drop. We use HTML5 DnD with a string payload of the conversation
  // id; folder rows accept the drop and call onMove. We intentionally
  // don't support drag-reordering within a folder yet — the spec just
  // calls for moving between folders.
  // ---------------------------------------------------------------------

  function onDragStart(e: DragEvent, id: string) {
    if (!e.dataTransfer) return;
    e.dataTransfer.setData("application/x-anyai-conv", id);
    e.dataTransfer.effectAllowed = "move";
  }

  function onDragOver(e: DragEvent, path: string) {
    if (!e.dataTransfer) return;
    if (!e.dataTransfer.types.includes("application/x-anyai-conv")) return;
    e.preventDefault();
    e.dataTransfer.dropEffect = "move";
    dragOverPath = path;
  }

  function onDragLeave(path: string) {
    if (dragOverPath === path) dragOverPath = null;
  }

  function onDrop(e: DragEvent, path: string) {
    e.preventDefault();
    dragOverPath = null;
    const id = e.dataTransfer?.getData("application/x-anyai-conv");
    if (!id) return;
    const item = items.find((c: ConversationMeta) => c.id === id);
    if (!item) return;
    if (item.path === path) return;
    onMove(id, path);
  }

  function toggleCollapsed(path: string) {
    const next = new Set(collapsed);
    if (next.has(path)) next.delete(path);
    else next.add(path);
    collapsed = next;
  }
</script>

<aside class="sidebar" class:open aria-hidden={!open}>
  <div class="head">
    <button class="new" onclick={onNew} title={newLabel}>
      <svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true">
        <path
          fill="currentColor"
          d="M12 5a1 1 0 0 1 1 1v5h5a1 1 0 1 1 0 2h-5v5a1 1 0 1 1-2 0v-5H6a1 1 0 1 1 0-2h5V6a1 1 0 0 1 1-1z"
        />
      </svg>
      <span>{newLabel}</span>
    </button>
    <button
      class="folder-btn"
      onclick={() => promptCreateFolder("")}
      title="New folder"
      aria-label="New folder"
    >
      <svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true">
        <path
          fill="currentColor"
          d="M10 4l2 2h6a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2h6zm6 8h-3V9h-2v3H8v2h3v3h2v-3h3v-2z"
        />
      </svg>
    </button>
    <button class="collapse" onclick={onClose} title="Hide sidebar" aria-label="Hide sidebar">
      <svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true">
        <path fill="currentColor" d="M14.7 6.3a1 1 0 0 1 0 1.4L10.4 12l4.3 4.3a1 1 0 1 1-1.4 1.4l-5-5a1 1 0 0 1 0-1.4l5-5a1 1 0 0 1 1.4 0z" />
      </svg>
    </button>
  </div>

  <div
    class="list"
    class:drop-root={dragOverPath === ""}
    onclick={closeMenu}
    role="presentation"
    ondragover={(e) => onDragOver(e, "")}
    ondragleave={() => onDragLeave("")}
    ondrop={(e) => onDrop(e, "")}
  >
    {#if items.length === 0 && folders.length === 0}
      <div class="empty">{emptyLabel}</div>
    {/if}

    {#if tree.items.length > 0}
      {#each groupByBand(tree.items) as group (group.label)}
        <div class="group-label">{group.label}</div>
        {#each group.rows as c (c.id)}
          {@render row(c, 0)}
        {/each}
      {/each}
    {/if}

    {#each tree.children as child (child.path)}
      {@render folder(child)}
    {/each}
  </div>
</aside>

{#snippet folder(node: Node)}
  {@const isCollapsed = collapsed.has(node.path)}
  <div
    class="folder"
    class:drop-target={dragOverPath === node.path}
    style="--depth: {node.depth};"
    role="button"
    tabindex="0"
    title={node.path}
    oncontextmenu={(e) => openFolderMenu(e, node.path)}
    ondragover={(e) => onDragOver(e, node.path)}
    ondragleave={() => onDragLeave(node.path)}
    ondrop={(e) => onDrop(e, node.path)}
    onclick={(e) => {
      e.stopPropagation();
      toggleCollapsed(node.path);
    }}
    onkeydown={(e) => {
      if (e.key === "Enter" || e.key === " ") {
        e.preventDefault();
        toggleCollapsed(node.path);
      }
    }}
  >
    <span class="folder-caret" aria-hidden="true">{isCollapsed ? "▸" : "▾"}</span>
    <svg class="folder-icon" viewBox="0 0 24 24" width="13" height="13" aria-hidden="true">
      <path
        fill="currentColor"
        d="M10 4l2 2h6a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2h6z"
      />
    </svg>
    {#if editingFolder === node.path}
      <input
        class="rename"
        bind:value={editValue}
        onblur={commitRenameFolder}
        onkeydown={(e) => onRenameKey(e, "folder")}
        onclick={(e) => e.stopPropagation()}
      />
    {:else}
      <span class="folder-name">{node.name}</span>
    {/if}
  </div>
  {#if !isCollapsed}
    {#each node.items as c (c.id)}
      {@render row(c, node.depth + 1)}
    {/each}
    {#each node.children as child (child.path)}
      {@render folder(child)}
    {/each}
  {/if}
{/snippet}

{#snippet row(c: ConversationMeta, depth: number)}
  <div
    class="row"
    class:active={c.id === activeId}
    style="--depth: {depth};"
    role="button"
    tabindex="0"
    draggable="true"
    onclick={() => onSelect(c.id)}
    onkeydown={(e) => {
      if (e.key === "Enter" || e.key === " ") {
        e.preventDefault();
        onSelect(c.id);
      }
    }}
    oncontextmenu={(e) => openItemMenu(e, c.id)}
    ondragstart={(e) => onDragStart(e, c.id)}
    title={c.title}
  >
    {#if c.mode === "transcribe"}
      <svg
        class="mode-icon"
        viewBox="0 0 24 24"
        width="11"
        height="11"
        aria-label="Transcription session"
      >
        <path
          fill="currentColor"
          d="M12 14a3 3 0 0 0 3-3V6a3 3 0 1 0-6 0v5a3 3 0 0 0 3 3zm5-3a5 5 0 0 1-10 0H5a7 7 0 0 0 6 6.92V21h2v-3.08A7 7 0 0 0 19 11h-2z"
        />
      </svg>
    {/if}
    {#if editingId === c.id}
      <input
        class="rename"
        bind:value={editValue}
        onblur={commitRenameItem}
        onkeydown={(e) => onRenameKey(e, "item")}
        onclick={(e) => e.stopPropagation()}
      />
    {:else}
      <span class="title">{c.title}</span>
    {/if}
  </div>
{/snippet}

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
    {#if menu.target.kind === "item"}
      {@const targetId = menu.target.id}
      <button onclick={() => startRenameItem(targetId)}>Rename</button>
      {@const item = items.find((c: ConversationMeta) => c.id === targetId)}
      {#if item && item.path}
        <button onclick={() => moveToRoot(targetId)}>Move to root</button>
      {/if}
      <button class="danger" onclick={() => deleteItemWithConfirm(targetId)}>Delete</button>
    {:else}
      {@const targetPath = menu.target.path}
      <button onclick={() => promptCreateFolder(targetPath)}>New subfolder</button>
      <button onclick={() => startRenameFolder(targetPath)}>Rename</button>
      <button class="danger" onclick={() => deleteFolderWithConfirm(targetPath)}>Delete</button>
    {/if}
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
  .folder-btn,
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
  .folder-btn:hover,
  .collapse:hover { background: #1a1a1a; color: #ccc; }
  .list {
    flex: 1;
    overflow-y: auto;
    padding: .35rem .25rem .5rem .25rem;
  }
  .list.drop-root {
    background: rgba(110, 110, 247, .04);
    box-shadow: inset 0 0 0 1px rgba(110, 110, 247, .35);
    border-radius: 8px;
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
  .folder {
    display: flex;
    align-items: center;
    gap: .35rem;
    padding: .35rem .45rem .35rem calc(.45rem + var(--depth, 0) * .9rem);
    margin: 1px 0;
    border-radius: 6px;
    color: #ccc;
    font-size: .8rem;
    cursor: pointer;
    user-select: none;
    transition: background .1s;
  }
  .folder:hover { background: #161616; }
  .folder.drop-target {
    background: rgba(110, 110, 247, .1);
    box-shadow: inset 0 0 0 1px #6e6ef7;
  }
  .folder-caret {
    width: 10px;
    color: #666;
    font-size: .7rem;
    text-align: center;
    flex-shrink: 0;
  }
  .folder-icon {
    color: #d4a64a;
    flex-shrink: 0;
  }
  .folder-name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    flex: 1;
  }
  .row {
    display: flex;
    align-items: center;
    gap: .35rem;
    padding: .4rem .55rem .4rem calc(.55rem + var(--depth, 0) * .9rem + .85rem);
    margin: 1px 0;
    border-radius: 6px;
    color: #bbb;
    font-size: .82rem;
    cursor: pointer;
    user-select: none;
    overflow: hidden;
    transition: background .1s, color .1s;
  }
  .row:hover { background: #161616; color: #e8e8e8; }
  .row.active { background: #1c1c2e; color: #fff; }
  .mode-icon {
    color: #6e6ef7;
    flex-shrink: 0;
  }
  .row .title {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    flex: 1;
  }
  .rename {
    flex: 1;
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
    min-width: 160px;
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
