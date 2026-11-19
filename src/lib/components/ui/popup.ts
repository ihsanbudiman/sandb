export type PopupAnchor = HTMLElement | { x: number; y: number };

type Placement = {
  anchor: PopupAnchor;
  align?: 'start' | 'end';
  matchWidth?: boolean;
  maxHeight?: number;
  dismiss: () => void;
};

/** Keep a fixed popup inside the window and out of the space occupied by its trigger. */
export function placePopup(node: HTMLElement, options: Placement) {
  const gutter = 8;
  const gap = 4;

  function place() {
    const { anchor, align, matchWidth, maxHeight = Infinity } = options;
    const { clientWidth: width, clientHeight: height } = document.documentElement;
    const bounds = anchor instanceof HTMLElement ? anchor.getBoundingClientRect() : {
      left: anchor.x, right: anchor.x, top: anchor.y, bottom: anchor.y, width: 0,
    };

    node.style.maxWidth = `${Math.max(0, width - 2 * gutter)}px`;
    node.style.minWidth = matchWidth ? `${Math.max(0, Math.min(bounds.width, width - 2 * gutter))}px` : '';
    // offsetWidth ignores the entry scale; scrollHeight includes rows clipped by the previous placement.
    const w = node.offsetWidth;
    const wantedHeight = Math.min(maxHeight, node.scrollHeight + node.offsetHeight - node.clientHeight);
    const below = Math.max(0, height - gutter - bounds.bottom - gap);
    const above = Math.max(0, bounds.top - gap - gutter);
    const flip = wantedHeight > below && above > below;
    const h = Math.min(wantedHeight, flip ? above : below);
    const x = align === 'end' ? bounds.right - w : bounds.left;
    const y = flip ? bounds.top - gap - h : bounds.bottom + gap;

    node.style.left = `${Math.max(gutter, Math.min(x, width - gutter - w))}px`;
    node.style.top = `${Math.max(gutter, Math.min(y, height - gutter - h))}px`;
    node.style.maxHeight = `${h}px`;
    node.style.transformOrigin = `${align === 'end' ? 'right' : 'left'} ${flip ? 'bottom' : 'top'}`;
  }

  function outside(e: PointerEvent) {
    const path = e.composedPath();
    if (!path.includes(node) && !(options.anchor instanceof HTMLElement && path.includes(options.anchor))) options.dismiss();
  }

  function onScroll(e: Event) {
    if (!(e.target instanceof Node) || !node.contains(e.target)) place();
  }

  const observer = new ResizeObserver(place);
  observer.observe(node);
  if (options.anchor instanceof HTMLElement) observer.observe(options.anchor);
  // The node's own box is capped by the last placement, so it does not resize when rows arrive later
  // (async lists, a filter that widens again). Re-place when the content changes; attributes are not
  // observed, because place() itself writes the node's style.
  let frame = 0;
  const content = new MutationObserver(() => {
    cancelAnimationFrame(frame);
    frame = requestAnimationFrame(place);
  });
  content.observe(node, { childList: true, subtree: true, characterData: true });
  place();
  document.addEventListener('pointerdown', outside, true);
  window.addEventListener('scroll', onScroll, true);
  window.addEventListener('resize', place);

  return {
    update(next: Placement) {
      if (options.anchor !== next.anchor) {
        if (options.anchor instanceof HTMLElement) observer.unobserve(options.anchor);
        if (next.anchor instanceof HTMLElement) observer.observe(next.anchor);
      }
      options = next;
      place();
    },
    destroy() {
      observer.disconnect();
      content.disconnect();
      cancelAnimationFrame(frame);
      document.removeEventListener('pointerdown', outside, true);
      window.removeEventListener('scroll', onScroll, true);
      window.removeEventListener('resize', place);
    },
  };
}
