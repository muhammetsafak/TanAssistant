/**
 * After a row is deleted the element that opened the confirmation is gone and focus falls to
 * <body>. Give it to `target` (a stable control such as the "add" button) in that case, so
 * keyboard users keep their place.
 */
export function refocusIfLost(target: HTMLElement | null): void {
  setTimeout(() => {
    const active = document.activeElement;
    if (!active || active === document.body || active.tagName === 'MAIN') {
      target?.focus();
    }
  }, 0);
}
