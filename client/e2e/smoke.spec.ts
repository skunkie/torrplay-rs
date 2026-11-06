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
