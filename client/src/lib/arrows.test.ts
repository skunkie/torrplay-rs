// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { type Box, nearest, startArrowNavigation } from "./arrows";

function box(left: number, top: number, width = 100, height = 40): Box {
  return { left, top, right: left + width, bottom: top + height };
}

describe("nearest", () => {
  // A row of three cards above a row of three.
  const grid = [box(0, 0), box(120, 0), box(240, 0), box(0, 100), box(120, 100), box(240, 100)];

  it.each([
    ["right along a row", 0, "right", 1],
    ["left along a row", 2, "left", 1],
    ["down a column", 1, "down", 4],
    ["up a column", 5, "up", 2],
  ] as const)("moves %s", (_case, from, direction, expected) => {
    const others = grid.map((held, index) => (index === from ? box(-9999, -9999) : held));
    expect(nearest(grid[from] as Box, direction, others)).toBe(expected);
  });

  it("stops at the edge of what there is", () => {
    expect(nearest(box(0, 0), "up", [box(0, 100)])).toBeUndefined();
    expect(nearest(box(0, 0), "left", [box(200, 0)])).toBeUndefined();
  });

  it("prefers what is straight ahead over what is nearer but aside", () => {
    const aside = box(300, 50);
    const ahead = box(0, 200);
    expect(nearest(box(0, 0), "down", [aside, ahead])).toBe(1);
  });

  it("moving sideways, prefers the far end of the row over something nearer below it", () => {
    expect(nearest(box(0, 0), "right", [box(120, 200), box(1800, 0)])).toBe(1);
  });

  it("moving sideways, stays in the row, however near something else is", () => {
    expect(nearest(box(0, 0), "right", [box(120, 200)])).toBeUndefined();
    expect(nearest(box(500, 0), "left", [box(0, -200)])).toBeUndefined();
  });

  it("goes to the nearest row when nothing is straight ahead, and the closest in it", () => {
    const farRow = box(400, 500);
    const row = [box(900, 100), box(300, 100)];
    expect(nearest(box(0, 0), "down", [farRow, ...row])).toBe(2);
  });

  it("goes only straight ahead when asked to", () => {
    expect(nearest(box(0, 0), "down", [box(500, 100)], true)).toBeUndefined();
  });

  it("still reaches what is only off to one side", () => {
    expect(nearest(box(0, 0), "down", [box(500, 100)])).toBe(0);
  });

  it("counts a box overlapping by a ring's width as past the edge", () => {
    expect(nearest(box(0, 0), "down", [box(0, 38)])).toBe(0);
  });

  it("does not count a box overlapping by more", () => {
    expect(nearest(box(0, 0), "down", [box(0, 20)])).toBeUndefined();
  });
});

describe("startArrowNavigation", () => {
  let stop: () => void;

  /** Gives the elements named boxes, since the test's document lays out nothing. */
  function place(boxes: Record<string, Box>): void {
    for (const [id, held] of Object.entries(boxes)) {
      const element = document.getElementById(id);
      if (!element) throw new Error(`no ${id}`);
      element.getBoundingClientRect = () =>
        ({ ...held, width: held.right - held.left, height: held.bottom - held.top }) as DOMRect;
    }
  }

  function press(key: string): KeyboardEvent {
    const event = new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true });
    (document.activeElement ?? document.body).dispatchEvent(event);
    return event;
  }

  beforeEach(() => {
    stop = startArrowNavigation();
  });

  afterEach(() => {
    stop();
    document.body.innerHTML = "";
  });

  it("starts from what the page marks, whatever has no focus yet", () => {
    document.body.innerHTML = `<main tabindex="-1"><a id="a" href="/a">A</a><a id="b" href="/b" data-focus-start>B</a></main>`;
    place({ a: box(0, 0), b: box(0, 100) });
    expect(press("ArrowDown").defaultPrevented).toBe(true);
    expect(document.activeElement?.id).toBe("b");
  });

  it("starts from the content rather than the header, on the confirming key too", () => {
    document.body.innerHTML = `<a id="h" href="/h">H</a><main tabindex="-1"><a id="a" href="/a">A</a></main>`;
    place({ h: box(0, 0), a: box(0, 100) });
    press("Enter");
    expect(document.activeElement?.id).toBe("a");
  });

  it("starts from what is on screen, having scrolled away from what the page marks", () => {
    document.body.innerHTML = `<main tabindex="-1"><a id="a" href="/a" data-focus-start>A</a><a id="b" href="/b">B</a></main>`;
    place({ a: box(0, -500), b: box(0, 100) });
    press("ArrowDown");
    expect(document.activeElement?.id).toBe("b");
  });

  it("leaves the key to the browser when nothing is on screen, so the page scrolls", () => {
    document.body.innerHTML = `<main tabindex="-1"><a id="a" href="/a">A</a></main>`;
    place({ a: box(0, -500) });
    expect(press("ArrowDown").defaultPrevented).toBe(false);
    expect(document.activeElement).toBe(document.body);
  });

  it("moves between elements, and leaves the key to the browser at the end", () => {
    document.body.innerHTML = `<a id="a" href="/a">A</a><a id="b" href="/b">B</a>`;
    place({ a: box(0, 0), b: box(0, 100) });
    document.getElementById("a")?.focus();
    expect(press("ArrowDown").defaultPrevented).toBe(true);
    expect(document.activeElement?.id).toBe("b");
    expect(press("ArrowDown").defaultPrevented).toBe(false);
    expect(document.activeElement?.id).toBe("b");
  });

  it("passes over what cannot take focus", () => {
    document.body.innerHTML = `<a id="a" href="/a">A</a><button id="off" disabled>Off</button><button id="roving" tabindex="-1">R</button><div inert><a id="inert" href="/i">I</a></div><a id="b" href="/b">B</a>`;
    place({
      a: box(0, 0),
      off: box(0, 60),
      roving: box(0, 70),
      inert: box(0, 80),
      b: box(0, 200),
    });
    document.getElementById("a")?.focus();
    press("ArrowDown");
    expect(document.activeElement?.id).toBe("b");
  });

  it("stays within a modal dialog", () => {
    document.body.innerHTML = `<a id="out" href="/o">O</a><dialog open><button id="a">A</button><button id="b">B</button></dialog>`;
    place({ out: box(0, 100), a: box(0, 0), b: box(120, 0) });
    document.getElementById("a")?.focus();
    press("ArrowDown");
    expect(document.activeElement?.id).toBe("a");
    press("ArrowRight");
    expect(document.activeElement?.id).toBe("b");
  });

  it("stays within an open menu", () => {
    document.body.innerHTML = `<a id="out" href="/o">O</a><div popover style="display: block"><button id="a">A</button></div>`;
    place({ out: box(0, 100), a: box(0, 0) });
    document.getElementById("a")?.focus();
    press("ArrowDown");
    expect(document.activeElement?.id).toBe("a");
  });

  it("leaves a text field up and down, and sideways only from the end of its text", () => {
    document.body.innerHTML = `<input id="field" value="ab" /><a id="right" href="/r">R</a><a id="below" href="/b">B</a>`;
    place({ field: box(0, 0), right: box(120, 0), below: box(0, 100) });
    const field = document.getElementById("field") as HTMLInputElement;
    field.focus();
    field.setSelectionRange(1, 1);
    expect(press("ArrowRight").defaultPrevented).toBe(false);
    expect(document.activeElement).toBe(field);
    field.setSelectionRange(2, 2);
    press("ArrowRight");
    expect(document.activeElement?.id).toBe("right");
    field.focus();
    field.setSelectionRange(1, 1);
    press("ArrowDown");
    expect(document.activeElement?.id).toBe("below");
  });

  it("leaves a text area up from its first line and down from its last", () => {
    document.body.innerHTML = `<a id="above" href="/a">A</a><textarea id="area"></textarea><a id="below" href="/b">B</a>`;
    place({ above: box(0, -100), area: box(0, 0), below: box(0, 100) });
    const area = document.getElementById("area") as HTMLTextAreaElement;
    area.value = "one\ntwo";
    area.focus();
    area.setSelectionRange(1, 1);
    press("ArrowDown");
    expect(document.activeElement).toBe(area);
    area.setSelectionRange(5, 5);
    press("ArrowUp");
    expect(document.activeElement).toBe(area);
    press("ArrowDown");
    expect(document.activeElement?.id).toBe("below");
    area.focus();
    area.setSelectionRange(1, 1);
    press("ArrowUp");
    expect(document.activeElement?.id).toBe("above");
  });

  describe("on a page with columns", () => {
    /** A menu column beside content, under a header, as the settings pages have. */
    function settings(): void {
      document.body.innerHTML = `
        <header><a id="logo" href="/">L</a><a id="profile" href="/p">P</a></header>
        <main id="main">
          <nav id="menu" data-arrows-column>
            <a id="one" href="/1">1</a>
            <a id="here" href="/2" aria-current="page">2</a>
            <a id="three" href="/3">3</a>
          </nav>
          <button id="top">T</button>
          <button id="bottom">B</button>
        </main>`;
      place({
        main: box(0, 0, 1200, 800),
        menu: box(0, 0, 100, 800),
        logo: box(0, -100),
        profile: box(1000, -100),
        one: box(0, 0),
        here: box(0, 50),
        three: box(0, 100),
        top: box(300, 20),
        bottom: box(300, 300),
      });
    }

    it("goes down within the content, past a menu item nearer below", () => {
      settings();
      document.getElementById("top")?.focus();
      press("ArrowDown");
      expect(document.activeElement?.id).toBe("bottom");
    });

    it("stays when the content ends, rather than going to the menu", () => {
      settings();
      document.getElementById("bottom")?.focus();
      press("ArrowDown");
      expect(document.activeElement?.id).toBe("bottom");
    });

    it("goes up from the top of the content to the header", () => {
      settings();
      document.getElementById("top")?.focus();
      press("ArrowUp");
      expect(document.activeElement?.id).toBe("logo");
    });

    it("goes left into the menu on the current section", () => {
      settings();
      document.getElementById("bottom")?.focus();
      press("ArrowLeft");
      expect(document.activeElement?.id).toBe("here");
    });

    it("lands on the current section from a row it shares with another", () => {
      settings();
      place({ bottom: box(300, 100) });
      document.getElementById("bottom")?.focus();
      press("ArrowLeft");
      expect(document.activeElement?.id).toBe("here");
    });

    it("goes down from the header to the content rather than a menu far aside", () => {
      settings();
      document.getElementById("profile")?.focus();
      press("ArrowDown");
      expect(document.activeElement?.id).toBe("top");
    });

    it("goes right from any menu item into the content", () => {
      settings();
      document.getElementById("three")?.focus();
      press("ArrowRight");
      expect(document.activeElement?.id).toBe("top");
    });

    it("never leads sideways from the content to the header", () => {
      settings();
      document.getElementById("top")?.focus();
      press("ArrowRight");
      expect(document.activeElement?.id).toBe("top");
    });

    it("stops at a column stacked between things, as on a narrow window", () => {
      document.body.innerHTML = `<main id="main"><button id="a">A</button><div id="column" data-arrows-column><button id="b">B</button></div><button id="c">C</button></main>`;
      place({
        main: box(0, 0, 600, 800),
        column: box(0, 100, 600, 40),
        a: box(0, 0),
        b: box(0, 100),
        c: box(0, 200),
      });
      document.getElementById("a")?.focus();
      press("ArrowDown");
      expect(document.activeElement?.id).toBe("b");
      press("ArrowDown");
      expect(document.activeElement?.id).toBe("c");
    });
  });

  it.each(["ArrowLeft", "ArrowDown"])("does not leave a text field on a held %s", (key) => {
    document.body.innerHTML = `<a id="left" href="/l">L</a><input id="field" /><a id="below" href="/b">B</a>`;
    place({ left: box(-200, 0), field: box(0, 0), below: box(0, 100) });
    document.getElementById("field")?.focus();
    const held = new KeyboardEvent("keydown", {
      key,
      repeat: true,
      bubbles: true,
      cancelable: true,
    });
    document.activeElement?.dispatchEvent(held);
    expect(document.activeElement?.id).toBe("field");
    press(key);
    expect(document.activeElement?.id).not.toBe("field");
  });

  it("does not let a dropdown change as focus passes, even with nowhere to go", () => {
    document.body.innerHTML = `<select id="s"><option>1</option><option>2</option></select>`;
    place({ s: box(0, 0) });
    document.getElementById("s")?.focus();
    expect(press("ArrowDown").defaultPrevented).toBe(true);
  });

  it.each([
    ["a tab", `<button id="w" role="tab">T</button>`, "ArrowRight"],
    ["a slider", `<div id="w" role="slider" tabindex="0"></div>`, "ArrowLeft"],
    ["a menu's item", `<div role="menu"><button id="w">I</button></div>`, "ArrowDown"],
    ["a radio button", `<input id="w" type="radio" />`, "ArrowDown"],
  ])("leaves %s its own arrow keys", (_case, html, key) => {
    document.body.innerHTML = `${html}<a id="o" href="/o">O</a>`;
    place({ w: box(0, 0), o: box(0, 100) });
    place({ o: key === "ArrowDown" ? box(0, 100) : box(key === "ArrowLeft" ? -200 : 200, 0) });
    document.getElementById("w")?.focus();
    expect(press(key).defaultPrevented).toBe(false);
    expect(document.activeElement?.id).toBe("w");
  });

  it("stands aside while the page takes the arrows, as the player does", () => {
    document.body.innerHTML = `<div data-arrows-taken><a id="a" href="/a">A</a></div>`;
    place({ a: box(0, 0) });
    expect(press("ArrowDown").defaultPrevented).toBe(false);
    expect(press("Enter").defaultPrevented).toBe(false);
    expect(document.activeElement).toBe(document.body);
  });

  it("ignores keys pressed with a modifier", () => {
    document.body.innerHTML = `<a id="a" href="/a">A</a>`;
    place({ a: box(0, 0) });
    document.body.dispatchEvent(
      new KeyboardEvent("keydown", { key: "ArrowDown", altKey: true, bubbles: true }),
    );
    expect(document.activeElement).toBe(document.body);
  });
});
