// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

// Moves focus with the arrow keys, as a remote's directional pad does. A remote has
// no Tab key, and a television's browser or a WebView may not move focus by itself.

export type Direction = "up" | "down" | "left" | "right";

/** Where an element is on the page. */
export interface Box {
  left: number;
  right: number;
  top: number;
  bottom: number;
}

const DIRECTIONS: Readonly<Record<string, Direction>> = {
  ArrowUp: "up",
  ArrowDown: "down",
  ArrowLeft: "left",
  ArrowRight: "right",
};

/** What could take focus. Whether each one can is for {@link focusable}. */
const CANDIDATES = "a[href],button,input,select,textarea,summary,[tabindex]";

/** How much distance across the direction counts against an element when crossing
 * to another column, compared with distance along it. */
const ACROSS_WEIGHT = 3;

/** How far two boxes may overlap and still count as one past the other, as
 * elements with a focus ring or a shared border do. */
const OVERLAP_PIXELS = 4;

/** The gap between two ranges on one axis, 0 where they overlap. */
function gap(fromStart: number, fromEnd: number, toStart: number, toEnd: number): number {
  return Math.max(0, toStart - fromEnd, fromStart - toEnd);
}

function isSideways(direction: Direction): boolean {
  return direction === "left" || direction === "right";
}

/** How far `box` is past `from`'s edge in `direction`, and how far off to the side. */
function distances(from: Box, direction: Direction, box: Box): { along: number; across: number } {
  const along =
    direction === "down"
      ? box.top - from.bottom
      : direction === "up"
        ? from.top - box.bottom
        : direction === "right"
          ? box.left - from.right
          : from.left - box.right;
  const across = isSideways(direction)
    ? gap(from.top, from.bottom, box.top, box.bottom)
    : gap(from.left, from.right, box.left, box.right);
  return { along, across };
}

/** The entry with the least `score`; the first among equals. */
function least<T>(entries: readonly T[], score: (entry: T) => number): T | undefined {
  let best: T | undefined;
  let bestScore = Infinity;
  for (const entry of entries) {
    const held = score(entry);
    if (held < bestScore) {
      best = entry;
      bestScore = held;
    }
  }
  return best;
}

/** The index of the box that moving `direction` from `from` reaches, if any. The
 * closest one straight ahead, if there is one. Left and right go no further than that,
 * so they stay in the row, as does `ahead`. Up and down otherwise go to the nearest row,
 * and the closest box in it. */
export function nearest(
  from: Box,
  direction: Direction,
  boxes: readonly Box[],
  ahead = false,
): number | undefined {
  const past = boxes
    .map((box, index) => ({ box, index, ...distances(from, direction, box) }))
    .filter(({ along }) => along >= -OVERLAP_PIXELS);
  const straight = least(
    past.filter(({ across }) => across === 0),
    ({ along }) => along,
  );
  if (straight || ahead || isSideways(direction)) return straight?.index;
  const closest = least(past, ({ along }) => along);
  if (!closest) return undefined;
  const row = past.filter(
    ({ box }) => gap(closest.box.top, closest.box.bottom, box.top, box.bottom) === 0,
  );
  return least(row, ({ across }) => across)?.index;
}

/** The index of the box to land on when crossing to another column, which need not
 * share a row with `from`. */
function nearestAcross(from: Box, direction: Direction, boxes: readonly Box[]): number | undefined {
  const past = boxes
    .map((box, index) => ({ index, ...distances(from, direction, box) }))
    .filter(({ along }) => along >= -OVERLAP_PIXELS);
  return least(past, ({ along, across }) => Math.max(0, along) + across * ACROSS_WEIGHT)?.index;
}

/** Whether the arrow keys can move focus to `element` now. */
function focusable(element: HTMLElement): boolean {
  if (element.tabIndex < 0 || (element as HTMLButtonElement).disabled) return false;
  // Holds focus for the player's hotkeys rather than being a thing to act on.
  if (element.localName === "media-container" || element.closest("[inert]")) return false;
  // Would keep left and right from the controls beyond it, and a remote has its own
  // volume keys. Tab still reaches it.
  if (element.closest("media-volume-slider")) return false;
  const box = element.getBoundingClientRect();
  return box.width > 0 && box.height > 0 && getComputedStyle(element).visibility !== "hidden";
}

/** Where `element` is, for moving focus: a slider's thumb counts as its whole bar,
 * since where the thumb happens to be says nothing of where the bar is. */
function boxOf(element: Element): Box {
  const bar = element.closest("media-time-slider");
  return (bar ?? element).getBoundingClientRect();
}

/** What focus may move among: what a modal dialog or open menu holds, or the page. */
function scope(active: Element | null): ParentNode {
  const dialogs = document.querySelectorAll("dialog[open]");
  const dialog = dialogs[dialogs.length - 1];
  return dialog ?? active?.closest("[popover]") ?? document;
}

function candidates(within: ParentNode): HTMLElement[] {
  return [...within.querySelectorAll<HTMLElement>(CANDIDATES)].filter(focusable);
}

/** Whether nothing a viewer acts on has focus, as when a page has just opened. */
function nothingFocused(active: Element | null): boolean {
  return !active || active === document.body || active.localName === "main";
}

/** What the page starts from: what it marks as its main action, or else the first
 * thing in its content, or else anything, among what is on screen, so a viewer who has
 * scrolled is not taken away from it. With nothing on screen, the arrow scrolls. */
function start(): HTMLElement | undefined {
  const all = candidates(scope(null)).filter((element) => {
    const box = element.getBoundingClientRect();
    return box.bottom > 0 && box.top < window.innerHeight;
  });
  return (
    all.find((element) => element.hasAttribute("data-focus-start")) ??
    all.find((element) => element.closest("main")) ??
    all[0]
  );
}

/** Whether a text field's caret still has somewhere to go in `direction`: text beside
 * it, or a line above or below it. A long line that wraps counts as one. */
function caretMoves(field: HTMLInputElement | HTMLTextAreaElement, direction: Direction): boolean {
  let at: number | null = null;
  let end: number | null = null;
  try {
    at = field.selectionStart;
    end = field.selectionEnd;
  } catch {
    // Some input types have no selection; there the arrows are free to leave.
  }
  if (at === null || end === null) return false;
  if (at !== end) return true;
  if (direction === "up") return field.value.slice(0, at).includes("\n");
  if (direction === "down") return field.value.slice(at).includes("\n");
  return direction === "left" ? at > 0 : at < field.value.length;
}

/** Whether `element` is somewhere to type. */
function isTextField(element: Element): boolean {
  if (element instanceof HTMLTextAreaElement) return true;
  return (
    element instanceof HTMLInputElement &&
    !["button", "checkbox", "file", "radio", "range", "submit"].includes(element.type)
  );
}

/** Whether `element` uses arrow keys in `direction` itself, so focus stays. */
function keepsArrow(element: Element, direction: Direction): boolean {
  const sideways = isSideways(direction);
  if (element.closest("media-menu, [role=menu], [role=listbox]")) return true;
  const role = element.getAttribute("role");
  if (sideways && (role === "tab" || role === "slider")) return true;
  if (element instanceof HTMLTextAreaElement) return caretMoves(element, direction);
  if (element instanceof HTMLInputElement) {
    if (element.type === "radio" || element.type === "range") return true;
    return sideways && caretMoves(element, direction);
  }
  return false;
}

/** The column `element` is in, if the page marks one and it stands beside something:
 * a narrow window stacks it with the content, and it is then simply more of the page. */
function columnOf(element: Element): Element | null {
  const column = element.closest("[data-arrows-column]");
  const parent = column?.parentElement;
  if (!column || !parent) return null;
  const stacked = column.getBoundingClientRect().width >= parent.getBoundingClientRect().width - 1;
  return stacked ? null : column;
}

function inContent(element: Element): boolean {
  return element.closest("main") !== null;
}

/** Where left or right goes from `active`: along its row, or else to another column.
 * Entering a column that has a current section lands on it. The header and the page's
 * content do not lead sideways to each other. */
function sidewaysFrom(
  active: Element,
  direction: Direction,
  all: readonly HTMLElement[],
): HTMLElement | undefined {
  const content = inContent(active);
  const level = all.filter((element) => inContent(element) === content);
  const from = boxOf(active);
  const column = columnOf(active);
  const inRow = nearest(from, direction, level.map(boxOf));
  let target = inRow === undefined ? undefined : level[inRow];
  if (!target && content) {
    const others = level.filter((element) => columnOf(element) !== column);
    const index = nearestAcross(from, direction, others.map(boxOf));
    target = index === undefined ? undefined : others[index];
  }
  const entered = target && columnOf(target) !== column ? columnOf(target) : null;
  return entered?.querySelector<HTMLElement>("[aria-current=page]") ?? target;
}

/** Where up or down goes from `active`. What is straight ahead in its own column or the
 * header comes first, then what is straight ahead anywhere, for a column laid out above
 * or below this one, and then the nearest row of its own column or the header. */
function verticallyFrom(
  active: Element,
  direction: Direction,
  all: readonly HTMLElement[],
): HTMLElement | undefined {
  const column = columnOf(active);
  const own = all.filter((element) => columnOf(element) === column || !inContent(element));
  const from = boxOf(active);
  for (const [pool, ahead] of [
    [own, true],
    [all, true],
    [own, false],
  ] as const) {
    const index = nearest(from, direction, pool.map(boxOf), ahead);
    if (index !== undefined) return pool[index];
  }
  return undefined;
}

function moveFocus(event: KeyboardEvent): void {
  const direction = DIRECTIONS[event.key];
  const active = document.activeElement;

  if (!direction) {
    // The confirming key of a remote, on a page where nothing has focus yet. In a
    // dropdown, it opens the list, which on some systems it would not by itself.
    if (event.key !== "Enter") return;
    if (active instanceof HTMLSelectElement) {
      try {
        active.showPicker();
        event.preventDefault();
      } catch {
        // Not there on older browsers, whose own handling of the key stands.
      }
    } else if (nothingFocused(active) && !document.querySelector("[data-arrows-taken]")) {
      const first = start();
      if (first) {
        event.preventDefault();
        first.focus();
      }
    }
    return;
  }
  if (document.querySelector("[data-arrows-taken]")) return;

  if (nothingFocused(active)) {
    const first = start();
    if (first) {
      event.preventDefault();
      first.focus();
    }
    return;
  }
  if (!active || keepsArrow(active, direction)) return;
  // A key held to move the caret, or to move through a page, stops at a field.
  if (event.repeat && isTextField(active)) return;

  const all = candidates(scope(active)).filter((element) => element !== active);
  const target = isSideways(direction)
    ? sidewaysFrom(active, direction, all)
    : verticallyFrom(active, direction, all);
  // With nowhere to go, the browser's own handling scrolls the page.
  if (target) target.focus();
  // A slider or dropdown would otherwise change as focus passes through it.
  if (target || active instanceof HTMLSelectElement || active.getAttribute("role") === "slider") {
    event.preventDefault();
    event.stopPropagation();
  }
}

/** Moves focus with the arrow keys from now on, until the returned function is
 * called. Listens first, so a slider or dropdown has no turn at a key used for this. */
export function startArrowNavigation(): () => void {
  const pressed = (event: KeyboardEvent) => {
    if (
      event.defaultPrevented ||
      event.altKey ||
      event.ctrlKey ||
      event.metaKey ||
      event.shiftKey
    ) {
      return;
    }
    moveFocus(event);
  };
  document.addEventListener("keydown", pressed, { capture: true });
  return () => document.removeEventListener("keydown", pressed, { capture: true });
}
