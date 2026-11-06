// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { test as base, expect, type Page } from "@playwright/test";

const TITLE = /test.pattern/i;

/** Fails a test when the content security policy refuses anything or a script
 * throws, which no unit test can see. */
const test = base.extend<{ clean: undefined }>({
  clean: [
    async ({ page }, use) => {
      const problems: string[] = [];
      page.on("pageerror", (error) => problems.push(error.message));
      page.on("console", (message) => {
        if (message.text().includes("Content Security Policy")) problems.push(message.text());
      });
      await use(undefined);
      expect(problems).toEqual([]);
    },
    { auto: true },
  ],
});

// The tests share one application, so what a failed test left must not fail
// the next one.
test.beforeEach(async ({ request }) => {
  const listed = await request.get("/api/v1/torrents", { params: { retention: "any" } });
  expect(listed.ok()).toBe(true);
  const { items } = (await listed.json()) as { items: { links: { self: string } }[] };
  for (const torrent of items) {
    expect((await request.delete(torrent.links.self)).ok()).toBe(true);
  }
});

function environment(name: string): string {
  const value = process.env[name];
  if (!value) throw new Error(`the rig sets ${name}`);
  return value;
}

async function deleteFromTitlePage(page: Page): Promise<void> {
  await page.getByRole("button", { name: "Delete", exact: true }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Delete", exact: true }).click();
  await expect(page).not.toHaveURL(/\/titles\//);
}

test("plays a magnet link from a peer and adds it to the library", async ({ page }) => {
  await page.goto("/add");
  await page.getByLabel("Magnet link").fill(environment("TORRPLAY_E2E_MAGNET"));

  // Its file list comes from the seeder, since a magnet link has none.
  await page.getByRole("button", { name: /^Play/ }).first().click({ timeout: 30_000 });

  const video = page.locator("video");
  await expect
    .poll(() => video.evaluate((media: HTMLVideoElement) => media.currentTime), {
      message: "the video plays",
      timeout: 30_000,
    })
    .toBeGreaterThan(1);

  // The bars fade while playing, and come back when the pointer moves.
  await page.mouse.move(300, 300);
  await page.getByRole("button", { name: "Add to library" }).click();
  await expect(page.getByText("In the library")).toBeVisible();
  expect(await video.evaluate((media: HTMLVideoElement) => media.paused)).toBe(false);

  await page.goto("/library");
  await page.getByRole("link", { name: TITLE }).click();
  await expect(page.getByRole("heading", { name: TITLE, level: 1 })).toBeVisible();

  await deleteFromTitlePage(page);
  await page.goto("/library");
  await expect(page.getByText("The library is empty.")).toBeVisible();
});

test("reads a torrent file and adds it to the library", async ({ page }) => {
  await page.goto("/add");
  await page.locator('input[type="file"]').setInputFiles(environment("TORRPLAY_E2E_TORRENT"));

  await expect(page.getByRole("heading", { name: TITLE, level: 1 })).toBeVisible();
  await page.getByRole("button", { name: "Add to library" }).click();

  await expect(page).toHaveURL(/\/titles\//);
  await deleteFromTitlePage(page);
});

test.describe("on a device whose system prefers dark", () => {
  test.use({ colorScheme: "dark" });

  test("keeps the theme chosen in the menu", async ({ page }) => {
    await page.goto("/library");
    const root = page.locator("html");
    await expect(root).toHaveAttribute("data-theme", "dark");

    await page.getByRole("button", { name: /^(Menu|Profile)/ }).click();
    await page.getByLabel("Theme").selectOption({ label: "Light" });
    await expect(root).toHaveAttribute("data-theme", "light");
    await expect(page.locator("body")).toHaveCSS("background-color", "rgb(248, 250, 252)");

    await page.reload();
    await expect(root).toHaveAttribute("data-theme", "light");
  });

  test("sets the theme without waiting for the client's bundle", async ({ page }) => {
    await page.goto("/library");
    await page.evaluate(() => localStorage.setItem("torrplay.theme", "light"));
    await page.route("**/assets/*.js", (route) => route.abort());

    await page.reload();

    await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
    await expect(page.locator('meta[name="theme-color"]')).toHaveAttribute("content", "#f8fafc");
  });
});

test.describe("settings", () => {
  test("saves a change the application keeps", async ({ page }) => {
    await page.goto("/library");
    await page.getByRole("button", { name: /^(Menu|Profile)/ }).click();
    await page.getByRole("link", { name: "Settings" }).click();
    await page.getByRole("link", { name: "Network and DLNA" }).click();
    await expect(page).toHaveURL("/settings/network");

    const peers = page.getByLabel("Peers per torrent");
    await expect(peers).toHaveValue("50");
    await peers.fill("80");
    await page.getByRole("button", { name: "Save" }).click();
    await expect(page.getByText("Saved")).toBeVisible();

    await page.reload();
    await expect(peers).toHaveValue("80");

    // The tests share one application.
    await peers.fill("50");
    await page.getByRole("button", { name: "Save" }).click();
    await expect(page.getByText("Saved")).toBeVisible();
  });

  test("saves with Enter", async ({ page }) => {
    await page.goto("/settings/network");
    const peers = page.getByLabel("Peers per torrent");
    await expect(peers).toHaveValue("50");

    await peers.fill("60");
    await peers.press("Enter");
    await expect(page.getByText("Saved")).toBeVisible();

    await peers.fill("50");
    await peers.press("Enter");
    await expect(page.getByText("Saved")).toBeVisible();
  });

  test("asks before a link leaves unsaved changes", async ({ page }) => {
    await page.goto("/settings/network");
    await page.getByLabel("Peers per torrent").fill("60");
    let asked = false;
    page.once("dialog", (dialog) => {
      asked = true;
      void dialog.dismiss();
    });

    await page.getByRole("link", { name: "TorrPlay" }).click();

    expect(asked).toBe(true);
    await expect(page).toHaveURL("/settings/network");
  });

  test("holds back a value out of bounds", async ({ page }) => {
    await page.goto("/settings/playback");
    const memory = page.getByLabel("Memory for pieces");
    await expect(memory).toHaveValue("256");

    await memory.fill("1");
    await expect(page.getByRole("button", { name: "Save" })).toBeDisabled();
    await expect(page.getByRole("alert")).toContainText("from 4 to 65536");
  });
});

test.describe("the demo", () => {
  test("runs the pages and plays its video without the API", async ({ page }) => {
    const reached: string[] = [];
    page.on("request", (request) => {
      const { pathname } = new URL(request.url());
      if (pathname.startsWith("/api/") || pathname === "/readyz") reached.push(pathname);
    });

    await page.goto("/demo");
    await expect(page.getByText("Demo", { exact: true })).toBeVisible();

    await page.getByRole("link", { name: "Library", exact: true }).first().click();
    await expect(page).toHaveURL("/demo/library");
    await page.getByRole("link", { name: /Glass Harbor/ }).click();
    await expect(page).toHaveURL(/\/demo\/titles\//);

    await page
      .getByRole("link", { name: /^(Resume|Play)/ })
      .first()
      .click();
    await expect(page).toHaveURL(/\/demo\/watch\//);
    await page.getByRole("button", { name: "Play now" }).click();
    const video = page.locator("video");
    const time = () => video.evaluate((media: HTMLVideoElement) => media.currentTime);
    // It resumes partway, so only moving on from there shows it plays.
    await expect.poll(time).toBeGreaterThan(0);
    const resumed = await time();
    await expect.poll(time, { message: "the sample plays" }).toBeGreaterThan(resumed);

    expect(reached).toEqual([]);
  });

  test("offers the sample's audio tracks and subtitles", async ({ page }) => {
    await page.goto("/demo");
    await page.getByRole("link", { name: "Resume" }).first().click();
    await page.getByRole("button", { name: "Play now" }).click();
    const video = page.locator("video");
    await expect
      .poll(() => video.evaluate((media: HTMLVideoElement) => media.currentTime))
      .toBeGreaterThan(10);

    await page.mouse.move(300, 300);
    await page.getByRole("button", { name: "Audio" }).click();
    await page.getByRole("menuitemradio", { name: /^Russian/ }).click();
    await page.mouse.move(310, 300);
    await page.getByRole("button", { name: "Subtitles" }).click();
    await page.getByRole("menuitemradio", { name: "English (SRT)" }).click();

    await expect
      .poll(() =>
        video.evaluate((media: HTMLVideoElement) => {
          const shown = [...media.textTracks].find((track) => track.mode === "showing");
          return shown?.cues?.length ?? 0;
        }),
      )
      .toBeGreaterThan(0);
    await expect(page.getByText("could not be played")).toBeHidden();
    expect(await video.evaluate((media: HTMLVideoElement) => media.paused)).toBe(false);
  });

  test("introduces itself once, and again when asked", async ({ page }) => {
    await page.goto("/demo");
    const heading = page.getByRole("heading", { name: "This is a demo of TorrPlay" });
    await expect(heading).toBeVisible();

    await page.getByRole("button", { name: "Got it" }).click();
    await expect(heading).toBeHidden();
    await page.reload();
    await expect(page.getByRole("link", { name: "Resume" }).first()).toBeVisible();
    await expect(heading).toBeHidden();

    await page.getByRole("button", { name: "Profile: Anna" }).click();
    await page.getByRole("button", { name: "About the demo" }).click();
    await expect(heading).toBeVisible();
  });

  test("adds the intro's sample magnet link as a magnet link would", async ({ page }) => {
    await page.goto("/demo");
    await page.getByRole("button", { name: "Add a torrent from a sample magnet link." }).click();

    await expect(page).toHaveURL("/demo/add");
    await expect(page.getByRole("heading", { name: /Night.Ferry/, level: 1 })).toBeVisible();
    // Its file arrives after a moment, as metadata does.
    await page.getByRole("button", { name: /^Play/ }).first().click();
    await expect(page).toHaveURL(/\/demo\/watch\//);
  });

  test("keeps a setting until the page reloads", async ({ page }) => {
    await page.goto("/demo/settings/network");
    const peers = page.getByLabel("Peers per torrent");
    await peers.fill("80");
    await page.getByRole("button", { name: "Save" }).click();
    await expect(page.getByText("Saved")).toBeVisible();

    await page.getByRole("link", { name: "Playback and cache" }).click();
    await expect(page).toHaveURL("/demo/settings/playback");
    await page.getByRole("link", { name: "Network and DLNA" }).click();
    await expect(peers).toHaveValue("80");

    await page.reload();
    await expect(peers).toHaveValue("50");
  });

  test("leaves the real profile and session alone", async ({ page }) => {
    await page.goto("/library");
    await page.evaluate(() => {
      localStorage.setItem("torrplay.profile", "prf_real");
      localStorage.setItem("torrplay.access-token", "secret");
    });

    await page.goto("/demo");
    await page.getByRole("button", { name: "Profile: Anna" }).click();
    await page.locator("#app-menu").getByLabel("Profile").selectOption({ label: "Ben" });
    await expect(page.getByRole("button", { name: "Profile: Ben" })).toBeVisible();

    const kept = await page.evaluate(() => [
      localStorage.getItem("torrplay.profile"),
      localStorage.getItem("torrplay.access-token"),
    ]);
    expect(kept).toEqual(["prf_real", "secret"]);
  });

  test("leaves through the menu, back to the real client", async ({ page }) => {
    await page.goto("/demo/library");
    await page.getByRole("button", { name: /^(Menu|Profile)/ }).click();
    await page.getByRole("link", { name: "Leave the demo" }).click();

    await expect(page).toHaveURL("/");
    await expect(page.getByText("Demo", { exact: true })).toBeHidden();
  });
});
