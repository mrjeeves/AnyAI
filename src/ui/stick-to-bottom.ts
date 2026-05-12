/** Svelte action: keep a scroll container pinned to its bottom while
 *  content streams in, unless the user has scrolled away.
 *
 *  Pass any reactive value as the parameter — every time it changes the
 *  action's `update` fires and we re-snap to the bottom *only if* the
 *  user was still at the bottom. A manual scroll-up flips the flag and
 *  suppresses auto-scroll until the user returns to the bottom.
 *
 *  Usage: `<div use:stickToBottom={transcript}>…</div>` */
export function stickToBottom(node: HTMLElement, _trigger?: unknown) {
  // ~one line of slack — sub-pixel rounding and the occasional 1px
  // rule make a strict equality check flaky.
  const SLOP = 16;

  function isAtBottom(): boolean {
    return node.scrollHeight - node.scrollTop - node.clientHeight <= SLOP;
  }

  let atBottom = isAtBottom();

  // Distinguish "user dragged the scrollbar" from our own scrollTop
  // writes. Without this guard, the programmatic write fires a scroll
  // event that recomputes `atBottom` against a layout that hasn't fully
  // settled yet, which can flip the flag off on slow paints.
  let programmatic = false;

  function onScroll() {
    if (programmatic) {
      programmatic = false;
      return;
    }
    atBottom = isAtBottom();
  }

  node.addEventListener("scroll", onScroll, { passive: true });

  return {
    update() {
      if (!atBottom) return;
      programmatic = true;
      node.scrollTop = node.scrollHeight;
    },
    destroy() {
      node.removeEventListener("scroll", onScroll);
    },
  };
}
