/** Svelte action: tag a scroll container with `data-overflow-up` /
 *  `data-overflow-down` attributes so styles can react to whether there
 *  is more content above or below the visible area.
 *
 *  The existing `.scroll-fade` utility already draws edge shadows, but
 *  on dense panels (e.g. the Families detail view) it isn't strong
 *  enough to read as "there's more below" — especially when the OS
 *  auto-hides the overlay scrollbar. Components opt into this action
 *  on top of `.scroll-fade` to render an explicit chevron hint, and
 *  they can hide the hint as soon as the user reaches an edge.
 *
 *  Usage:
 *    `<div class="scroll-fade" use:scrollAffordance>…</div>`
 *
 *  Then style off the data attributes:
 *    `[data-overflow-down="true"] .my-hint { opacity: 1; }`
 */
export function scrollAffordance(node: HTMLElement) {
  const SLOP = 4;

  function update() {
    const canUp = node.scrollTop > SLOP;
    const canDown =
      node.scrollHeight - node.scrollTop - node.clientHeight > SLOP;
    node.dataset.overflowUp = canUp ? "true" : "false";
    node.dataset.overflowDown = canDown ? "true" : "false";
  }

  update();
  node.addEventListener("scroll", update, { passive: true });

  // Content can grow or shrink after mount (inline pull progress, error
  // banners, lazy mode blocks). Without a ResizeObserver the hint sticks
  // at its initial state and stops matching what's actually scrollable.
  const ro = new ResizeObserver(update);
  ro.observe(node);
  for (const child of Array.from(node.children)) {
    ro.observe(child);
  }

  return {
    destroy() {
      node.removeEventListener("scroll", update);
      ro.disconnect();
    },
  };
}
