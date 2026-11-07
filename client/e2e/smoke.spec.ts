// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { test as base, expect, type Locator, type Page } from "@playwright/test";

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

test("keeps a refused magnet link's way back on one line on a phone in Russian", async ({
  page,
}) => {
  await page.setViewportSize({ width: 360, height: 760 });
  await page.addInitScript(() => localStorage.setItem("torrplay.locale", "ru"));
  await page.route("**/api/v1/torrents", (route) =>
    route.request().method() === "POST"
      ? route.fulfill({
          status: 422,
          contentType: "application/problem+json",
          body: JSON.stringify({
            type: "tag:skunkie.github.io,2026:torrplay/problems/validation-failed",
            title: "Validation failed",
            status: 422,
            errors: [],
          }),
        })
      : route.continue(),
  );
  await page.goto("/add");
  await page.getByLabel("Magnet-ссылка").fill(environment("TORRPLAY_E2E_MAGNET"));
  await page.getByRole("button", { name: "Добавить в библиотеку" }).click({ timeout: 30_000 });

  const back = page.getByRole("alert").getByRole("button", { name: "Выбрать другой торрент" });
  await expect(back).toBeVisible();
  const box = await back.boundingBox();
  expect(box?.height, "the link takes one line").toBeLessThan(30);
  const scrollWidth = await page.evaluate(() => document.documentElement.scrollWidth);
  expect(scrollWidth, "nothing to scroll sideways").toBe(360);
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

test.describe("stremio", () => {
  test("gives a link that Stremio can read", async ({ page, request }) => {
    await page.goto("/settings");
    await page.getByRole("link", { name: "Stremio" }).click();
    await expect(page).toHaveURL("/settings/stremio");

    // The tests reach the application on loopback, and it listens only there.
    const link = page.getByLabel("Link for Stremio");
    await expect(page.getByText(/works on this computer only/)).toBeVisible();
    const manifest = await request.get(await link.inputValue());
    expect(manifest.ok()).toBe(true);
    expect(await manifest.json()).toMatchObject({ name: expect.any(String) });
  });

  test("creates a link that holds its own key while a password is set", async ({
    page,
    request,
  }) => {
    const PASSWORD = "an invented test passphrase";
    expect((await request.put("/api/v1/password", { data: { password: PASSWORD } })).ok()).toBe(
      true,
    );
    try {
      await page.goto("/settings/stremio");
      await page.getByLabel("Password", { exact: true }).fill(PASSWORD);
      await page.getByRole("button", { name: "Sign in" }).click();
      await expect(page).toHaveURL("/settings/stremio");

      await page.getByLabel("Current password").fill(PASSWORD);
      await page.getByRole("button", { name: "Create link" }).click();
      const link = page.getByLabel("Link for Stremio");
      await expect(link).toBeFocused();

      // Without a credential of its own, as Stremio asks.
      const url = await link.inputValue();
      expect(url).toMatch(/\/stremio\/[^/]+\/manifest\.json$/);
      expect((await request.get(url)).ok()).toBe(true);
      const bare = url.replace(/\/stremio\/[^/]+\//, "/stremio/");
      expect((await request.get(bare)).status()).toBe(401);

      await page.reload();
      await expect(page.getByRole("heading", { name: "Existing links" })).toBeVisible();
      await page.getByRole("button", { name: "Revoke" }).click();
      await page.getByRole("dialog").getByRole("button", { name: "Revoke" }).click();
      await expect(page.getByRole("button", { name: "Create link" })).toBeVisible();
      expect((await request.get(url)).status()).toBe(401);
    } finally {
      // Whatever went wrong, the other tests share this application.
      const cleanup = await request.post("/api/v1/access-tokens", {
        data: { password: PASSWORD, name: "Cleanup", scope: "api" },
      });
      const { secret } = (await cleanup.json()) as { secret: string };
      const headers = { Authorization: `Bearer ${secret}` };
      expect((await request.delete("/api/v1/password", { headers })).ok()).toBe(true);
    }
  });
});

test.describe("system", () => {
  test("shows the run, finds a record, and changes the log level", async ({ page }) => {
    await page.goto("/settings");
    await page.getByRole("link", { name: "System" }).click();
    await expect(page).toHaveURL("/settings/system");

    await expect(page.getByRole("region", { name: "Build" })).toContainText("TorrPlay");
    await expect(page.getByRole("img", { name: /evictions/ })).toBeVisible();

    await page.getByLabel("Search the log").fill("the API is listening");
    const records = page.getByRole("listitem");
    await expect(records.first()).toContainText("the API is listening");

    // Responses that succeed are debug messages, so only this level shows them.
    const level = page.getByLabel("Log level", { exact: true });
    await expect(level).toHaveValue("info");
    await level.selectOption("debug");
    await page.getByLabel("Lowest level shown").selectOption("debug");
    await page.getByLabel("Search the log").fill("/api/v1/log-records");
    // The next poll's requests are logged at the new level at the latest.
    await expect(records.first()).toContainText("response", { timeout: 15_000 });

    await page.reload();
    await expect(level).toHaveValue("debug");

    // The tests share one application.
    const saved = page.waitForResponse(
      (response) =>
        response.url().endsWith("/api/v1/settings") && response.request().method() === "PATCH",
    );
    await level.selectOption("info");
    await saved;
    await page.reload();
    await expect(level).toHaveValue("info");
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

  test("asks before the back button leaves unsaved changes", async ({ page }) => {
    await page.goto("/");
    await page.getByRole("button", { name: /^(Menu|Profile)/ }).click();
    await page.getByRole("link", { name: "Settings" }).click();
    const memory = page.getByLabel("Memory for pieces");
    const held = await memory.inputValue();
    await memory.fill(String(Number(held) + 1));

    let accept = false;
    page.on("dialog", (dialog) => void (accept ? dialog.accept() : dialog.dismiss()));
    await page.goBack({ waitUntil: "commit" }).catch(() => undefined);
    await expect(page).toHaveURL("/settings");
    await expect(memory).toHaveValue(String(Number(held) + 1));

    accept = true;
    await page.goBack({ waitUntil: "commit" }).catch(() => undefined);
    await expect(page).toHaveURL("/");
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

/** A one-pixel PNG, the smallest image the server takes as one. */
const PIXEL = Buffer.from(
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M/wHwAEBgIApD5fRAAAAABJRU5ErkJggg==",
  "base64",
);

/** Whether the row of the profile `name` shows an avatar that really loaded. */
function avatarLoaded(page: Page, name: string): Promise<boolean> {
  return page
    .locator("li", { hasText: name })
    .locator("img")
    .evaluate((image: HTMLImageElement) => image.complete && image.naturalWidth > 0);
}

test.describe("profiles", () => {
  test("adds a person with an avatar, and deletes them", async ({ page }) => {
    await page.goto("/settings/profiles");
    await page.getByLabel("New profile").fill("Cleo");
    await page.getByRole("button", { name: "Add" }).click();
    await expect(page.getByRole("main").getByText("Cleo", { exact: true })).toBeVisible();

    await page
      .getByLabel("Choose an avatar for Cleo")
      .setInputFiles({ name: "cleo.png", mimeType: "image/png", buffer: PIXEL });
    await expect(page.getByRole("button", { name: "Remove the avatar of Cleo" })).toBeVisible();
    await expect.poll(() => avatarLoaded(page, "Cleo")).toBe(true);

    await page.getByRole("button", { name: "Delete Cleo" }).click();
    await page.getByRole("dialog").getByRole("button", { name: "Delete" }).click();
    await expect(page.getByRole("main").getByText("Cleo", { exact: true })).toBeHidden();
  });

  test("shows an avatar through a playback token while a password is set", async ({
    page,
    request,
  }) => {
    const PASSWORD = "an invented test passphrase";
    const profile = (await (await request.get("/api/v1/profiles")).json()) as {
      items: { links: { avatar: string }; kind: string; name: string }[];
    };
    const person = profile.items.find((item) => item.kind === "person");
    if (!person) throw new Error("the application has a person");
    const put = await request.put(person.links.avatar, {
      headers: { "Content-Type": "image/png" },
      data: PIXEL,
    });
    expect(put.ok()).toBe(true);
    expect((await request.put("/api/v1/password", { data: { password: PASSWORD } })).ok()).toBe(
      true,
    );
    try {
      await page.goto("/settings/profiles");
      await page.getByLabel("Password").fill(PASSWORD);
      await page.getByRole("button", { name: "Sign in" }).click();
      await expect(page).toHaveURL("/settings/profiles");

      await expect(page.getByRole("main").getByText(person.name, { exact: true })).toBeVisible();
      await expect.poll(() => avatarLoaded(page, person.name)).toBe(true);
      const source = await page
        .locator("li", { hasText: person.name })
        .locator("img")
        .getAttribute("src");
      expect(source).toContain("token=");
    } finally {
      const signedIn = await request.post("/api/v1/access-tokens", {
        data: { password: PASSWORD, name: "Cleanup", scope: "api" },
      });
      const { secret } = (await signedIn.json()) as { secret: string };
      const headers = { Authorization: `Bearer ${secret}` };
      await request.delete(person.links.avatar, { headers });
      expect((await request.delete("/api/v1/password", { headers })).ok()).toBe(true);
    }
  });
});

test.describe("security", () => {
  test("sets, changes, and removes the password, and creates tokens that work", async ({
    page,
    request,
  }) => {
    const FIRST = "an invented first passphrase";
    const SECOND = "an invented second passphrase";
    let password = FIRST;
    try {
      await page.goto("/settings/security");
      await page.getByLabel("New password", { exact: true }).fill(FIRST);
      await page.getByLabel("Repeat the new password").fill(FIRST);
      await page.getByRole("button", { name: "Set password" }).click();

      // Stays signed in, on this page, with a token it knows for its own.
      await expect(page.getByText("This device")).toBeVisible();
      await expect(page).toHaveURL("/settings/security");
      await page.reload();
      await expect(page.getByText("This device")).toBeVisible();

      await page.getByRole("button", { name: "New token" }).click();
      await page.getByLabel("Name", { exact: true }).fill("Scraper");
      await page.getByLabel("Password", { exact: true }).fill(FIRST);
      await page.getByLabel("Scope").selectOption("metrics");
      await page.getByRole("button", { name: "Create" }).click();
      const secret = await page.getByLabel("The new token").inputValue();
      const headers = { Authorization: `Bearer ${secret}` };
      expect((await request.get("/api/v1/statistics", { headers })).ok()).toBe(true);
      expect((await request.get("/api/v1/settings", { headers })).status()).toBe(403);

      await page.getByRole("button", { name: "Done" }).click();
      await page.getByRole("button", { name: "Revoke Scraper" }).click();
      await page.getByRole("dialog").getByRole("button", { name: "Revoke" }).click();
      await expect(page.getByRole("button", { name: "Revoke Scraper" })).toBeHidden();
      expect((await request.get("/api/v1/statistics", { headers })).status()).toBe(401);

      await page.getByRole("button", { name: "Change password" }).click();
      await page.getByLabel("Current password").fill(FIRST);
      await page.getByLabel("New password", { exact: true }).fill(SECOND);
      await page.getByLabel("Repeat the new password").fill(SECOND);
      await page.getByLabel(/Sign out every device/).check();
      await page.getByRole("main").getByRole("button", { name: "Change password" }).click();
      password = SECOND;
      await expect(page.getByLabel("New password", { exact: true })).toBeHidden();
      await expect(page.getByText("This device")).toBeVisible();
      await expect(page).toHaveURL("/settings/security");

      await page.getByRole("button", { name: "Remove password" }).click();
      await page.getByRole("dialog").getByRole("button", { name: "Remove password" }).click();
      await expect(page.getByText(/anyone on this network/)).toBeVisible();
      await expect(page).toHaveURL("/settings/security");
    } finally {
      // Whatever went wrong, the other tests share this application.
      const signedIn = await request.post("/api/v1/access-tokens", {
        data: { password, name: "Cleanup" },
      });
      if (signedIn.ok()) {
        const { secret } = (await signedIn.json()) as { secret: string };
        await request.delete("/api/v1/password", {
          headers: { Authorization: `Bearer ${secret}` },
        });
      }
    }
    expect((await request.get("/api/v1/password")).status()).toBe(404);
  });
});

test.describe("backup", () => {
  test("downloads a backup, and restores it", async ({ page, request }) => {
    await page.goto("/settings/backup");
    const [download] = await Promise.all([
      page.waitForEvent("download"),
      page.getByRole("button", { name: "Download" }).click(),
    ]);
    expect(download.suggestedFilename()).toMatch(/\.zip$/);
    const archive = await download.path();

    await page.getByLabel("Choose a file").setInputFiles(archive);
    await page.getByRole("dialog").getByRole("button", { name: "Restore" }).click();
    await expect(page.getByText(/^Restored\./)).toBeVisible();
    await expect.poll(async () => (await request.get("/readyz")).status()).toBe(200);
  });

  test("refuses a file that is not a backup", async ({ page }) => {
    await page.goto("/settings/backup");
    await page
      .getByLabel("Choose a file")
      .setInputFiles({ name: "notes.zip", mimeType: "application/zip", buffer: PIXEL });
    await page.getByRole("dialog").getByRole("button", { name: "Restore" }).click();
    await expect(page.getByRole("alert")).toContainText("not a backup");
  });
});

test.describe("on the real server", () => {
  test("shows a poster at once when it is replaced", async ({ page }) => {
    await page.goto("/add");
    await page.locator('input[type="file"]').setInputFiles(environment("TORRPLAY_E2E_TORRENT"));
    await page.getByRole("button", { name: "Add to library" }).click();
    await expect(page).toHaveURL(/\/titles\//);

    const setPoster = async (): Promise<string> => {
      await page.getByRole("link", { name: "Edit" }).click();
      await page.getByLabel("Poster").setInputFiles({
        name: "poster.png",
        mimeType: "image/png",
        buffer: PIXEL,
      });
      await page.getByRole("button", { name: "Save" }).click();
      await expect(page).toHaveURL(/\/titles\/[^/]+$/);
      const poster = page.locator("[data-hero] img").last();
      await expect
        .poll(() => poster.evaluate((image: HTMLImageElement) => image.naturalWidth))
        .toBeGreaterThan(0);
      return (await poster.getAttribute("src")) ?? "";
    };

    const first = await setPoster();
    // Its upload time, to the second, is what makes a new image a new URL.
    await page.waitForTimeout(1100);
    expect(await setPoster()).not.toBe(first);
    await deleteFromTitlePage(page);
  });

  test("goes back to its place in a list that takes time to read", async ({ page }) => {
    await page.goto("/add");
    await page.locator('input[type="file"]').setInputFiles(environment("TORRPLAY_E2E_TORRENT"));
    await page.getByRole("button", { name: "Add to library" }).click();
    await expect(page).toHaveURL(/\/titles\//);
    await page.route("**/api/v1/torrents?*", async (route) => {
      await new Promise((resolve) => setTimeout(resolve, 400));
      await route.fallback();
    });
    await page.setViewportSize({ width: 320, height: 240 });
    await page.goto("/library");
    const card = page.locator("a[href*='/titles/']").first();
    await card.waitFor();
    await page.evaluate(() => window.scrollTo(0, 150));
    const place = await page.evaluate(() => window.scrollY);
    expect(place).toBeGreaterThan(50);

    // Not a pointer's click, which would scroll the card into view first.
    await card.evaluate((link: HTMLElement) => link.click());
    await expect(page).toHaveURL(/\/titles\//);
    await expect
      .poll(() => page.evaluate(() => window.scrollY), "a new page starts at its top")
      .toBe(0);
    await page.goBack();
    await card.waitFor();

    await expect.poll(() => page.evaluate(() => window.scrollY)).toBe(place);
    await page.unroute("**/api/v1/torrents?*");
  });

  test("ends a page with the build, except on a phone, which has it on the System page", async ({
    page,
    request,
  }) => {
    const system = (await (await request.get("/api/v1/system")).json()) as {
      version: string;
      commit: string | null;
      commit_date: string | null;
    };
    // This is a checkout, so the build must have found its commit.
    expect(system.commit).toMatch(/^[0-9a-f]{7,}(-dirty)?$/);
    expect(system.commit_date).toMatch(/^\d{4}-\d{2}-\d{2}$/);
    const footer = page.locator("footer");

    // A page shorter than the window: one line, at the window's bottom.
    await page.setViewportSize({ width: 1280, height: 800 });
    await page.goto("/settings/backup");
    await expect(footer).toHaveText(`TorrPlay ${system.version} · ${system.commit ?? ""}`);
    expect((await footer.locator("..").boundingBox())?.height).toBe(800);
    const box = await footer.boundingBox();
    expect(Math.round((box?.y ?? 0) + (box?.height ?? 0))).toBe(800);
    expect(box?.height).toBeLessThan(48);

    // A phone: not there, and on the System page instead.
    await page.setViewportSize({ width: 360, height: 640 });
    await expect(footer).toBeHidden();
    await page.goto("/settings/system");
    await expect(
      page.getByRole("region", { name: "Build" }).getByText(system.commit ?? ""),
    ).toBeVisible();

    // Not over the player.
    await page.setViewportSize({ width: 1280, height: 800 });
    await page.goto("/demo/watch/trr_aaaaaaaaaaaaaaaaaaaaaaaaa1/fil_aaaaaaaaaaaaaaaaaaaaaa1003");
    await expect(page.getByRole("button", { name: "Play now" })).toBeVisible();
    await expect(footer).toHaveCount(0);
  });

  test("keeps text whole on a small phone, in the longer language", async ({ page }) => {
    await page.setViewportSize({ width: 360, height: 800 });
    await page.addInitScript(() => localStorage.setItem("torrplay.locale", "ru"));
    const title = "/demo/titles/trr_aaaaaaaaaaaaaaaaaaaaaaaaa1";

    // A poster's badge inside its card, and its title not cut.
    await page.goto("/demo/library");
    const badge = page.getByText("Приостановлен");
    const card = badge.locator("xpath=ancestor::a");
    const badgeBox = await badge.boundingBox();
    const cardBox = await card.boundingBox();
    expect((badgeBox?.x ?? 0) + (badgeBox?.width ?? 0)).toBeLessThanOrEqual(
      (cardBox?.x ?? 0) + (cardBox?.width ?? 0),
    );
    const caption = page.getByText("The Lighthouse Keepers", { exact: true }).last();
    expect(await caption.evaluate((held) => held.scrollHeight <= held.clientHeight)).toBe(true);

    // An episode's file keeps the end of its name, where episodes differ.
    await page.goto(title);
    const files = page.locator("bdi");
    await expect(files.first()).toBeVisible();
    const shownEnd = await files.first().evaluate((held) => {
      const range = document.createRange();
      range.selectNodeContents(held);
      return (
        range.getBoundingClientRect().right -
        (held.parentElement?.getBoundingClientRect().right ?? 0)
      );
    });
    expect(Math.abs(shownEnd)).toBeLessThan(2);

    // Every kind on screen, and the page's actions together.
    await page.goto(`${title}/edit`);
    const other = page.getByRole("button", { name: "Другое" });
    expect(
      ((await other.boundingBox())?.x ?? 0) + ((await other.boundingBox())?.width ?? 0),
    ).toBeLessThanOrEqual(360);
    const cancel = await page.getByRole("link", { name: "Отмена" }).boundingBox();
    const save = await page.getByRole("button", { name: "Сохранить" }).boundingBox();
    expect(cancel?.y).toBe(save?.y);

    // A number stays with its unit, and a row of parts starts with no dot.
    await page.goto("/demo/settings/security");
    await expect(page.getByText(/3\u00A0часа назад/).first()).toBeVisible();
    await page.goto("/demo/settings/system");
    const value = page.getByText("Активных: 1", { exact: true });
    expect((await value.boundingBox())?.height).toBeLessThan(36);
    const parts = page.getByRole("region", { name: "Сборка" }).locator(".dotted");
    await expect(parts.first()).toContainText("TorrPlay");
    const rows = await parts.evaluateAll((held) => {
      const clip = held[0]?.parentElement?.parentElement as HTMLElement;
      const box = clip.getBoundingClientRect();
      const tops = held.map((part) => part.getBoundingClientRect().top);
      return {
        count: new Set(tops).size,
        clipped: getComputedStyle(clip).overflowX === "hidden",
        rightEdge: Math.max(...held.map((part) => part.getBoundingClientRect().right)) - box.right,
        // How much of each dot's space lies inside the clip, for those starting a row.
        dotShown: held.flatMap((part, index) =>
          tops[index] === tops[index - 1]
            ? []
            : [
                part.getBoundingClientRect().left +
                  parseFloat(getComputedStyle(part, "::before").width) -
                  box.left,
              ],
        ),
      };
    });
    expect(rows.count).toBeGreaterThan(1);
    expect(rows.clipped).toBe(true);
    expect(rows.rightEdge).toBeLessThanOrEqual(1);
    expect(Math.max(...rows.dotShown)).toBeLessThanOrEqual(1);

    // It copies as one line, for a bug report.
    const copied = await page.getByRole("region", { name: "Сборка" }).evaluate((section) => {
      const parts = section.querySelector(".dotted")?.parentElement as HTMLElement;
      const selection = getSelection();
      selection?.selectAllChildren(parts);
      return selection?.toString() ?? "";
    });
    expect(copied).toMatch(/^TorrPlay.+ · .+ · .+ · .+$/);
    expect(copied).not.toContain("\n");

    // A screen reader gets one dot between parts, and none before the first.
    const read = await page.getByRole("region", { name: "Сборка" }).ariaSnapshot();
    expect(read.match(/·/g)).toHaveLength(3);

    // A part with no spaces wraps inside the box instead of being cut off.
    const longPart = await parts.first().evaluate((part) => {
      part.textContent = "The.Invented.Very.Long.Lighthouse.Keepers.S01.1080p.WEB-DL.x265";
      return (
        part.getBoundingClientRect().right -
        (part.parentElement?.parentElement?.getBoundingClientRect().right ?? 0)
      );
    });
    expect(longPart).toBeLessThanOrEqual(1);

    // The narrowest phones still keep a tile's value on one line.
    await page.setViewportSize({ width: 320, height: 800 });
    expect((await value.boundingBox())?.height).toBeLessThan(36);
    const speed = page.getByText(/МиБ\/с$/).first();
    expect((await speed.boundingBox())?.height).toBeLessThan(36);
  });

  test.describe("on the preload screen", () => {
    const WATCH = "/demo/watch/trr_aaaaaaaaaaaaaaaaaaaaaaaaa1/fil_aaaaaaaaaaaaaaaaaaaaaa1003";

    /** Opens it with the demo's clock held, so its preload stays on screen. */
    async function open(page: Page, width: number, height: number) {
      await page.clock.setFixedTime(Date.now());
      await page.setViewportSize({ width, height });
      await page.goto(WATCH);
      await expect(page.getByRole("button", { name: "Play now" })).toBeVisible();
      return page.locator("section[data-hero]");
    }

    for (const [shape, width, height] of [
      ["tall", 360, 640],
      ["wide", 667, 375],
    ] as const) {
      test(`shows the whole poster in a ${shape} phone's window`, async ({ page }) => {
        const hero = await open(page, width, height);
        const poster = await hero.locator("img").last().boundingBox();
        const heading = await hero.getByRole("heading").boundingBox();
        const back = await hero.getByRole("link", { name: "Back" }).boundingBox();
        const button = await hero.getByRole("button", { name: "Play now" }).boundingBox();
        if (!poster || !heading || !back || !button) throw new Error("not laid out");

        // On top, not dimmed by the hero's shade.
        const onTop = await page.evaluate(
          ([x, y]) => document.elementFromPoint(x ?? 0, y ?? 0)?.tagName,
          [poster.x + poster.width / 2, poster.y + poster.height / 2],
        );
        expect(onTop).toBe("IMG");
        expect(poster.width / poster.height).toBeCloseTo(2 / 3, 1);
        expect(poster.x).toBeGreaterThanOrEqual(0);
        expect(poster.x + poster.width).toBeLessThanOrEqual(width);
        expect(poster.y + poster.height).toBeLessThanOrEqual(height);
        expect(button.y + button.height).toBeLessThanOrEqual(height);
        if (shape === "tall") {
          expect(poster.y).toBeGreaterThanOrEqual(back.y + back.height);
          expect(poster.y + poster.height).toBeLessThanOrEqual(heading.y);
          expect(poster.height).toBeGreaterThan(height / 3);
        } else {
          expect(poster.x).toBeGreaterThanOrEqual(heading.x + heading.width);
          expect(poster.height).toBeGreaterThan(height / 2);
        }
      });
    }

    test("keeps the poster above the text in a nearly square window", async ({ page }) => {
      const hero = await open(page, 420, 400);
      const poster = await hero.locator("img").last().boundingBox();
      const heading = await hero.getByRole("heading").boundingBox();
      const button = await hero.getByRole("button", { name: "Play now" }).boundingBox();
      expect((poster?.y ?? 0) + (poster?.height ?? 0)).toBeLessThanOrEqual(heading?.y ?? 0);
      expect(heading?.width).toBeGreaterThan(300);
      expect(button?.height).toBeLessThan(60);
    });

    test("falls back to the title's color when the poster fails to load", async ({ page }) => {
      await page.route("**/*", (route) =>
        route.request().resourceType() === "image" ? route.abort() : route.continue(),
      );
      const hero = await open(page, 390, 844);
      await expect(hero.locator("img")).toHaveCount(0);
      const background = await hero.evaluate((held) => held.style.background);
      expect(background).not.toBe("");
    });

    test("keeps a long title clear of the back link in a short window", async ({ page }) => {
      for (const [width, height] of [
        [844, 390],
        [568, 320],
      ] as const) {
        const hero = await open(page, width, height);
        await hero
          .getByRole("heading")
          .evaluate((held) => (held.textContent = "An Invented Title ".repeat(4).trim()));
        const back = await hero.getByRole("link", { name: "Back" }).boundingBox();
        const label = await hero.locator("p").first().boundingBox();
        const button = await hero.getByRole("button", { name: "Play now" }).boundingBox();
        expect(label?.y).toBeGreaterThanOrEqual((back?.y ?? 0) + (back?.height ?? 0));
        expect((button?.y ?? 0) + (button?.height ?? 0)).toBeLessThanOrEqual(height);
      }
    });
  });

  test("keeps the search box in one place on Home and Library", async ({ page }) => {
    await page.setViewportSize({ width: 1280, height: 800 });
    await page.goto("/demo");
    const box = page.getByRole("banner").getByRole("searchbox");
    const atHome = await box.boundingBox();
    await page.getByRole("link", { name: "Library" }).first().click();
    await expect(page).toHaveURL(/\/demo\/library/);
    expect(await box.boundingBox()).toEqual(atHome);

    // There is one box, and it searches the page.
    await expect(page.getByRole("searchbox")).toHaveCount(1);
    await box.fill("orchard");
    await expect(page).toHaveURL(/q=orchard/);
    await expect(page.getByRole("link", { name: /Orchard Road/ })).toBeVisible();
    await expect(page.getByRole("link", { name: /Glass Harbor/ })).toHaveCount(0);
    await page.reload();
    await expect(box).toHaveValue("orchard");

    // A phone has no box in its header, so the page has one.
    await page.setViewportSize({ width: 390, height: 844 });
    await expect(page.getByRole("banner").getByRole("searchbox")).toHaveCount(0);
    await expect(page.getByRole("main").getByRole("searchbox")).toHaveValue("orchard");
  });

  test("signing out revokes the device's token", async ({ page, request }) => {
    const PASSWORD = "an invented test passphrase";
    expect((await request.put("/api/v1/password", { data: { password: PASSWORD } })).ok()).toBe(
      true,
    );
    try {
      await page.goto("/");
      await page.getByLabel("Password").fill(PASSWORD);
      await page.getByRole("button", { name: "Sign in" }).click();
      await expect(page).toHaveURL("/");
      const secret = await page.evaluate(() => localStorage.getItem("torrplay.access-token"));
      const headers = { Authorization: `Bearer ${secret}` };
      expect((await request.get("/api/v1/settings", { headers })).ok()).toBe(true);

      await page.getByRole("button", { name: /^(Menu|Profile)/ }).click();
      await page.getByRole("button", { name: "Sign out" }).click();

      await expect(page).toHaveURL(/\/sign-in/);
      expect((await request.get("/api/v1/settings", { headers })).status()).toBe(401);
    } finally {
      const signedIn = await request.post("/api/v1/access-tokens", {
        data: { password: PASSWORD, name: "Cleanup", scope: "api" },
      });
      const { secret } = (await signedIn.json()) as { secret: string };
      const headers = { Authorization: `Bearer ${secret}` };
      expect((await request.delete("/api/v1/password", { headers })).ok()).toBe(true);
    }
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

  test("answers the keyboard as soon as a video opens", async ({ page }) => {
    await page.goto("/demo");
    await page.locator("a[href*='/watch/']").first().click();
    const video = page.locator("video");
    const skip = page.getByRole("button", { name: "Play now" });
    await expect(video.or(skip)).toBeVisible();
    if (await skip.isVisible()) await skip.click();
    await expect(video).toBeVisible();
    const paused = () => video.evaluate((element: HTMLVideoElement) => element.paused);
    await expect.poll(paused).toBe(false);

    await page.keyboard.press("k");

    await expect.poll(paused).toBe(true);
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

  test("remuxes the sample where the browser cannot play Matroska, as Safari cannot", async ({
    page,
  }) => {
    await page.addInitScript(() => {
      const answer = HTMLMediaElement.prototype.canPlayType;
      HTMLMediaElement.prototype.canPlayType = function (type: string) {
        return /^video\/(x-matroska|webm)/.test(type) ? "" : answer.call(this, type);
      };
    });
    await page.goto("/demo");
    await page.getByRole("link", { name: "Resume" }).first().click();
    await page.getByRole("button", { name: "Play now" }).click();
    const video = page.locator("video");
    const time = () => video.evaluate((media: HTMLVideoElement) => media.currentTime);
    await expect.poll(time, { message: "the remuxed sample plays" }).toBeGreaterThan(10);
    expect(await video.evaluate((media: HTMLVideoElement) => media.src)).toMatch(/^blob:/);

    // Back to the start and on again, within what is buffered or past it.
    await video.evaluate((media: HTMLVideoElement) => (media.currentTime = 1));
    await expect.poll(time).toBeLessThan(5);
    await expect.poll(time, { message: "it plays on after a seek" }).toBeGreaterThan(3);
    await video.evaluate((media: HTMLVideoElement) => (media.currentTime = 25));
    await expect.poll(time, { message: "it plays on near the end" }).toBeGreaterThan(26);

    await page.mouse.move(300, 300);
    await page.getByRole("button", { name: "Audio" }).click();
    await page.getByRole("menuitemradio", { name: /^Russian/ }).click();
    await video.evaluate((media: HTMLVideoElement) => (media.currentTime = 2));
    await expect.poll(time).toBeGreaterThan(4);
    await expect(page.getByText("could not be played")).toBeHidden();
    await expect(page.locator("media-error-dialog")).toBeHidden();
    // Still remuxed, rather than handed the file after a failure.
    expect(await video.evaluate((media: HTMLVideoElement) => media.src)).toMatch(/^blob:/);
  });

  test("keeps a long title to one line where a large window has the room", async ({ page }) => {
    await page.setViewportSize({ width: 1920, height: 1080 });
    await page.goto("/demo/titles/trr_aaaaaaaaaaaaaaaaaaaaaaaaa1/edit");
    await page.getByLabel(/^Title/).fill("A long invented title ".repeat(5).trim());
    await page.getByRole("button", { name: "Save" }).click();

    const heading = page.getByRole("heading", { level: 1 });
    await expect(heading).toHaveText(/^A long invented title/);
    const lines = await heading.evaluate((element) => {
      const text = document.createRange();
      text.selectNodeContents(element);
      return new Set([...text.getClientRects()].map(({ top }) => Math.round(top))).size;
    });
    expect(lines).toBe(1);
  });

  test("shows a long title whole on its page and short of that on the home page", async ({
    page,
  }) => {
    await page.setViewportSize({ width: 1280, height: 800 });
    await page.goto("/demo/titles/trr_aaaaaaaaaaaaaaaaaaaaaaaaa1/edit");
    await page.getByLabel(/^Title/).fill("A very long invented title ".repeat(12));
    await page.getByRole("button", { name: "Save" }).click();

    const heading = page.getByRole("heading", { level: 1 });
    await expect(heading).toHaveText(/^A very long invented title/);
    const cut = (element: HTMLElement) => element.scrollHeight > element.clientHeight;
    expect(await heading.evaluate(cut), "all of it shows").toBe(false);
    const textWidth = await heading.evaluate((element) => {
      const text = document.createRange();
      text.selectNodeContents(element);
      return text.getBoundingClientRect().width;
    });
    expect(textWidth, "it takes the width beside the poster").toBeGreaterThan(768);

    await page.getByRole("link", { name: "Back" }).click();
    await expect(heading).toHaveText(/^A very long invented title/);
    expect(await heading.evaluate(cut), "on the home page it stops short").toBe(true);
  });

  test.describe("a title being played, with every piece showing", () => {
    const THE_TITLE = "/demo/titles/trr_aaaaaaaaaaaaaaaaaaaaaaaaa1";

    /** Opens every piece, returning the strip's width from before that. */
    async function showPieces(page: Page): Promise<number | undefined> {
      await page.goto(THE_TITLE);
      const strip = page.getByRole("button", { name: /show all pieces/ });
      const width = (await strip.boundingBox())?.width;
      await strip.click();
      await expect(page.getByRole("list", { name: "Readers" })).toBeVisible();
      return width;
    }

    async function expectSameWidth(page: Page, before: number | undefined): Promise<void> {
      expect(before, "the strip was measured").toBeGreaterThan(0);
      const after = await page.getByRole("button", { name: /hide all pieces/ }).boundingBox();
      expect(after?.width, "the strip keeps its size").toBe(before);
      const scrollWidth = await page.evaluate(() => document.documentElement.scrollWidth);
      expect(scrollWidth, "nothing to scroll sideways").toBe(page.viewportSize()?.width);
    }

    test("shows all of the reader's file name on a phone", async ({ page }) => {
      await page.setViewportSize({ width: 375, height: 812 });
      const before = await showPieces(page);
      const name = page.getByRole("list", { name: "Readers" }).getByText("S01E04");
      const clipped = await name.evaluate((text) => text.scrollWidth > text.clientWidth);
      expect(clipped, "the end of the name names the episode").toBe(false);
      await expectSameWidth(page, before);
    });

    test("keeps its width for a long file name on a tablet", async ({ page }) => {
      await page.setViewportSize({ width: 768, height: 1024 });
      const before = await showPieces(page);
      // Longer than the window, which no name in the demo is.
      await page
        .getByRole("list", { name: "Readers" })
        .getByText("S01E04")
        .evaluate((text) => (text.textContent = "A.Very.Long.File.Name.".repeat(20)));
      await expectSameWidth(page, before);
    });
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

  test("keeps every page within its window, at any width, in either language", async ({
    browser,
  }) => {
    test.setTimeout(180_000);
    const overflowing: string[] = [];
    for (const locale of ["en-US", "ru"]) {
      const context = await browser.newContext();
      await context.addInitScript(
        (value) => localStorage.setItem("torrplay.locale", value),
        locale,
      );
      const page = await context.newPage();
      await page.goto("/demo/library");
      const title = await page.locator("a[href*='/titles/']").first().getAttribute("href");
      const paths = [
        "/demo",
        "/demo/library",
        "/demo/add",
        "/demo/settings",
        ...["playback", "network", "stremio", "profiles", "security", "backup", "system"].map(
          (section) => `/demo/settings/${section}`,
        ),
        "/demo/sign-in",
        `${title}`,
        `${title}/edit`,
      ];
      for (const width of [320, 375, 768, 820, 1024, 1280]) {
        await page.setViewportSize({ width, height: 800 });
        for (const path of paths) {
          await page.goto(path);
          await page.locator("main > *").first().waitFor();
          const over = await page.evaluate(
            () => document.documentElement.scrollWidth - document.documentElement.clientWidth,
          );
          if (over > 0) overflowing.push(`${locale} ${width} ${path}: ${over} px`);
        }
      }
      await context.close();
    }
    expect(overflowing).toEqual([]);
  });

  test("shows posters it serves itself, and a tile for a torrent with none", async ({ page }) => {
    await page.goto("/demo/library");
    const poster = page.locator('img[src^="/demo-media/posters/paper-moons.webp"]').first();
    await expect(poster).toBeVisible();
    await expect
      .poll(() => poster.evaluate((image: HTMLImageElement) => image.naturalWidth))
      .toBeGreaterThan(0);
    await expect(page.getByText("Orchard Road")).toBeVisible();
  });

  test("shows a hero's poster whole, beside its content", async ({ page }) => {
    for (const path of ["/demo", "/demo/titles/trr_aaaaaaaaaaaaaaaaaaaaaaaaa1"]) {
      await page.goto(path);
      const hero = page.locator("section[data-hero]");
      const title = hero.getByText("The Lighthouse Keepers").first();
      await expect(title).toBeVisible();
      // The last image: the first holds only the poster's colors, behind.
      const box = await hero.locator("img").last().boundingBox();
      const words = await title.boundingBox();
      if (!box || !words) throw new Error(`${path} has a poster and a title`);

      expect(box.width, path).toBeGreaterThan(100);
      expect(box.width / box.height, path).toBeCloseTo(2 / 3, 1);
      expect(words.x + words.width, path).toBeLessThanOrEqual(box.x);
    }
  });

  test("revokes a token, and offers a backup that restores", async ({ page }) => {
    await page.goto("/demo/settings/security");
    await page.getByRole("button", { name: "Revoke Living room TV" }).click();
    await page.getByRole("dialog").getByRole("button", { name: "Revoke" }).click();
    await expect(page.getByText("Living room TV")).toBeHidden();

    await page.getByRole("link", { name: "Backup" }).click();
    const [download] = await Promise.all([
      page.waitForEvent("download"),
      page.getByRole("button", { name: "Download" }).click(),
    ]);
    await page.getByLabel("Choose a file").setInputFiles(await download.path());
    await page.getByRole("dialog").getByRole("button", { name: "Restore" }).click();
    await expect(page.getByText(/^Restored\./)).toBeVisible();
  });

  test("adds a person with an avatar until the page reloads", async ({ page }) => {
    await page.goto("/demo/settings/profiles");
    await page.getByLabel("New profile").fill("Cleo");
    await page.getByRole("button", { name: "Add" }).click();

    await page
      .getByLabel("Choose an avatar for Cleo")
      .setInputFiles({ name: "cleo.png", mimeType: "image/png", buffer: PIXEL });
    await expect.poll(() => avatarLoaded(page, "Cleo")).toBe(true);

    await page.reload();
    await expect(page.getByRole("main").getByText("Anna", { exact: true })).toBeVisible();
    await expect(page.getByRole("main").getByText("Cleo", { exact: true })).toBeHidden();
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

test.describe("with only a remote's arrows and confirming key", () => {
  const THE_TITLE = "/demo/titles/trr_aaaaaaaaaaaaaaaaaaaaaaaaa1";

  test.use({ viewport: { width: 1920, height: 1080 } });

  /** A short description of what has focus. */
  function focused(page: Page): Promise<string> {
    return page.evaluate(() => {
      const element = document.activeElement;
      return (
        element?.getAttribute("aria-label") ??
        element?.textContent?.trim().slice(0, 30) ??
        element?.localName ??
        ""
      );
    });
  }

  for (const width of [1920, 900, 600]) {
    for (const path of [
      "/demo",
      "/demo/library",
      "/demo/add",
      THE_TITLE,
      `${THE_TITLE}/edit`,
      "/demo/settings/playback",
      "/demo/settings/network",
      "/demo/settings/stremio",
      "/demo/settings/profiles",
      "/demo/settings/security",
      "/demo/settings/backup",
      "/demo/settings/system",
    ]) {
      test(`reaches everything on ${path} at ${width} pixels`, async ({ page }) => {
        test.setTimeout(240_000);
        await page.setViewportSize({ width, height: 800 });
        await page.goto(path);
        await expect(
          page.locator("main").locator("a:visible, button:visible").first(),
        ).toBeVisible();

        // Tags what Tab reaches, then walks from one to the next by arrows alone.
        const total = await page.evaluate(() => {
          const all = [
            ...document.querySelectorAll<HTMLElement>(
              "a[href],button,input,select,textarea,summary,[tabindex]",
            ),
          ].filter((element) => {
            const box = element.getBoundingClientRect();
            return (
              element.tabIndex >= 0 &&
              !(element as HTMLButtonElement).disabled &&
              box.width > 0 &&
              box.height > 0 &&
              getComputedStyle(element).visibility !== "hidden"
            );
          });
          all.forEach((element, index) => element.setAttribute("data-walk", String(index)));
          return all.length;
        });
        expect(total).toBeGreaterThan(0);

        await page.keyboard.press("ArrowDown");
        const reached = new Set<string>();
        const queue: string[] = [];
        const note = async () => {
          const id = await page.evaluate(
            () => document.activeElement?.getAttribute("data-walk") ?? null,
          );
          if (id !== null && !reached.has(id)) {
            reached.add(id);
            queue.push(id);
          }
        };
        await note();
        while (queue.length > 0) {
          const from = queue.shift();
          for (const key of ["ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight"]) {
            // Moving between season tabs replaces the episodes under them.
            const sideways = key === "ArrowLeft" || key === "ArrowRight";
            if (sideways && (await page.locator(`[data-walk="${from}"][role=tab]`).count()) > 0) {
              continue;
            }
            await page.locator(`[data-walk="${from}"]`).evaluate((element: HTMLElement) => {
              element.focus();
              // A field's caret at its end, which arrows leave from.
              if (element instanceof HTMLInputElement && element.type !== "file") {
                try {
                  element.setSelectionRange(element.value.length, element.value.length);
                } catch {
                  // This type of field has no caret.
                }
              }
            });
            await page.keyboard.press(key);
            await note();
          }
        }
        const unreached = await page.evaluate(
          (held) =>
            [...document.querySelectorAll<HTMLElement>("[data-walk]")]
              .filter((element) => !held.includes(element.getAttribute("data-walk") ?? ""))
              .map(
                (element) => element.getAttribute("aria-label") ?? element.outerHTML.slice(0, 80),
              ),
          [...reached],
        );
        expect(unreached).toEqual([]);
      });
    }
  }

  test("moves where a viewer expects, not only somewhere", async ({ page }) => {
    const cases: {
      path: string;
      from: (page: Page) => Locator;
      key: string;
      to: ((page: Page) => Locator) | "stays";
    }[] = [
      {
        // Past the panel's height, across the season's tab, to what is above.
        path: THE_TITLE,
        from: (held) => held.getByRole("link", { name: /^2\b/ }),
        key: "ArrowUp",
        to: (held) => held.getByRole("tab", { name: "Season 1" }),
      },
      {
        path: "/demo/library",
        from: (held) => held.locator('main a[href*="/titles/"]').last(),
        key: "ArrowRight",
        to: "stays",
      },
      {
        path: "/demo/settings/security",
        from: (held) => held.getByRole("button", { name: "Change password" }),
        key: "ArrowDown",
        to: (held) => held.getByRole("button", { name: "New token" }),
      },
      {
        path: "/demo/settings/security",
        from: (held) => held.getByRole("button", { name: /^Revoke/ }).first(),
        key: "ArrowLeft",
        to: (held) => held.getByRole("link", { name: "Security" }),
      },
      {
        path: "/demo/settings/security",
        from: (held) => held.getByRole("button", { name: /^Revoke/ }).first(),
        key: "ArrowRight",
        to: "stays",
      },
      {
        path: "/demo/settings/security",
        from: (held) => held.getByRole("button", { name: "Change password" }),
        key: "ArrowLeft",
        to: (held) => held.getByRole("link", { name: "Security" }),
      },
      {
        path: "/demo/settings/security",
        // The closest of the content's first row, not the list of sections far to the left.
        from: (held) => held.getByRole("button", { name: /^Profile/ }),
        key: "ArrowDown",
        to: (held) => held.getByRole("button", { name: "Remove password" }),
      },
      {
        path: "/demo/settings/stremio",
        from: (held) => held.getByRole("button", { name: "Create link" }),
        key: "ArrowDown",
        to: "stays",
      },
      {
        path: `${THE_TITLE}/edit`,
        from: (held) => held.getByRole("button", { name: "Choose a poster" }),
        key: "ArrowLeft",
        to: "stays",
      },
      {
        path: `${THE_TITLE}/edit`,
        from: (held) => held.getByRole("button", { name: "Choose a poster" }),
        key: "ArrowUp",
        to: (held) => held.getByRole("link", { name: "Back" }),
      },
    ];
    for (const { path, from, key, to } of cases) {
      await page.goto(path);
      const start = from(page);
      await start.focus();
      await page.keyboard.press(key);
      await expect(to === "stays" ? start : to(page), `${path} ${key}`).toBeFocused();
    }
  });

  test("starts each page on its main action", async ({ page }) => {
    await page.goto("/demo/library");
    await expect(page.locator('main a[href*="/titles/"]').first()).toBeVisible();
    await page.keyboard.press("ArrowDown");
    await expect(page.locator('main a[href*="/titles/"]').first()).toBeFocused();

    await page.goto(THE_TITLE);
    await expect(page.getByRole("link", { name: /^(Resume|Play)/ })).toBeVisible();
    await page.keyboard.press("Enter");
    await expect(page.getByRole("link", { name: /^(Resume|Play)/ })).toBeFocused();
    await expect(page).toHaveURL(THE_TITLE);
  });

  test("comes back to the card it opened, by the back button and by the page's arrow", async ({
    page,
  }) => {
    await page.goto("/demo/library");
    const card = page.locator('main a[href*="/titles/"]').nth(2);
    await card.focus();
    await page.keyboard.press("Enter");
    await expect(page).toHaveURL(/\/demo\/titles\//);
    await page.goBack();
    await expect(card).toBeFocused();

    await page.keyboard.press("Enter");
    await expect(page).toHaveURL(/\/demo\/titles\//);
    await page.getByRole("link", { name: "Back" }).focus();
    await page.keyboard.press("Enter");
    await expect(page).toHaveURL("/demo/library");
    await expect(card).toBeFocused();
  });

  test("opens the app menu, moves within it, and returns to its button", async ({ page }) => {
    await page.goto("/demo/library");
    const button = page.getByRole("button", { name: /^Profile/ });
    await button.focus();
    await page.keyboard.press("Enter");
    const menu = page.locator("#app-menu");
    await expect(menu.locator("select").first()).toBeFocused();
    const language = await menu.getByLabel("Language").inputValue();
    await page.keyboard.press("ArrowDown");
    await expect(menu.getByLabel("Language")).toBeFocused();
    await page.keyboard.press("ArrowDown");
    await expect(menu.getByLabel("Language")).toHaveValue(language);
    for (let press = 0; press < 8; press++) await page.keyboard.press("ArrowDown");
    expect(await menu.evaluate((held) => held.contains(document.activeElement))).toBe(true);
    await page.keyboard.press("Escape");
    await expect(button).toBeFocused();
  });

  test("passes through the Library's dropdowns without changing them", async ({ page }) => {
    await page.goto("/demo/library");
    const select = page.locator("main select").first();
    await select.focus();
    await page.keyboard.press("ArrowDown");
    await expect(select).toHaveValue("");
    await expect(select).not.toBeFocused();
    await expect(page).toHaveURL("/demo/library");
  });

  test("opens a dropdown with the confirming key and chooses in it", async ({ page }) => {
    await page.goto("/demo/library");
    const select = page.locator("main select").first();
    await select.focus();
    await page.keyboard.press("Enter");
    await page.keyboard.press("ArrowDown");
    await page.keyboard.press("Enter");
    await expect(select).toHaveValue("Family");
    await expect(select).toBeFocused();
  });

  test("moves between a confirmation's buttons and stays in it", async ({ page }) => {
    await page.goto(THE_TITLE);
    await page.getByRole("button", { name: "Delete", exact: true }).focus();
    await page.keyboard.press("Enter");
    const dialog = page.getByRole("dialog");
    await expect(dialog.getByRole("button", { name: "Keep it" })).toBeFocused();
    await page.keyboard.press("ArrowRight");
    await expect(dialog.getByRole("button", { name: "Delete" })).toBeFocused();
    await page.keyboard.press("ArrowUp");
    await expect(dialog.getByRole("button", { name: "Delete" })).toBeFocused();
    await page.keyboard.press("ArrowLeft");
    await expect(dialog.getByRole("button", { name: "Keep it" })).toBeFocused();
  });

  test("plays, pauses, seeks, reaches every control, and leaves", async ({ page }) => {
    test.setTimeout(120_000);
    await page.goto(THE_TITLE);
    await expect(page.getByRole("link", { name: /^(Resume|Play)/ })).toBeVisible();
    await page.keyboard.press("Enter");
    await page.keyboard.press("Enter");
    await expect(page).toHaveURL(/\/demo\/watch\//);
    await expect(page.getByRole("button", { name: "Play now" })).toBeFocused();
    await page.keyboard.press("Enter");

    const video = page.locator("video");
    await expect(video).toBeVisible();
    const media = (read: (media: HTMLMediaElement) => number | boolean) => video.evaluate(read);
    const rewind = () =>
      video.evaluate((held: HTMLMediaElement) => {
        held.currentTime = 0;
      });
    const bars = page.locator("media-controls");
    await expect.poll(() => media((held) => !held.paused)).toBe(true);
    await rewind();

    // The bars closed: left and right seek, the confirming key pauses.
    await page.keyboard.press("ArrowRight");
    await expect.poll(() => media((held) => held.currentTime >= 9)).toBe(true);
    await page.keyboard.press("ArrowLeft");
    await expect.poll(() => media((held) => held.currentTime < 9)).toBe(true);
    await page.keyboard.press("Enter");
    await expect.poll(() => media((held) => held.paused)).toBe(true);
    await page.keyboard.press("Enter");
    await expect.poll(() => media((held) => !held.paused)).toBe(true);

    // Up or down opens the bars instead of changing the volume.
    const volume = await media((held) => held.volume);
    await rewind();
    await page.keyboard.press("ArrowUp");
    await expect(page.locator("media-play-button")).toBeFocused();
    await expect(bars).toHaveAttribute("data-held", "true");
    expect(await media((held) => held.volume)).toBe(volume);

    // Left and right now move among the controls, and don't seek.
    await rewind();
    await page.keyboard.press("ArrowRight");
    await expect(page.locator("media-seek-button").last()).toBeFocused();
    expect(await media((held) => held.currentTime < 5)).toBe(true);
    // Stays in the row rather than falling to the controls below.
    await page.keyboard.press("ArrowRight");
    await expect(page.locator("media-seek-button").last()).toBeFocused();
    await page.keyboard.press("ArrowLeft");
    await page.keyboard.press("ArrowUp");
    await expect(page.getByRole("link", { name: "Back" })).toBeFocused();
    await page.keyboard.press("ArrowDown");
    const seekBar = page.getByRole("slider", { name: "Seek" });
    await expect(seekBar).toBeFocused();
    await page.keyboard.press("ArrowUp");
    await expect(page.locator("media-play-button")).toBeFocused();
    await page.keyboard.press("ArrowDown");
    await expect(seekBar).toBeFocused();

    // The seek bar takes left and right, by the same step as the rest, wherever it is.
    await video.evaluate((held: HTMLMediaElement) => {
      held.pause();
      held.currentTime = 13;
    });
    await expect.poll(() => media((held) => Math.round(held.currentTime))).toBe(13);
    await page.keyboard.press("ArrowRight");
    await expect.poll(() => media((held) => Math.round(held.currentTime))).toBe(23);
    await page.keyboard.press("ArrowLeft");
    await page.keyboard.press("ArrowLeft");
    await expect.poll(() => media((held) => Math.round(held.currentTime))).toBe(3);
    await video.evaluate((held: HTMLMediaElement) => void held.play());
    await rewind();

    await page.keyboard.press("ArrowDown");
    await expect(page.locator("media-mute-button")).toBeFocused();
    for (const name of ["Audio", "Subtitles"]) {
      await page.keyboard.press("ArrowRight");
      await expect(page.getByRole("button", { name })).toBeFocused();
    }
    await page.keyboard.press("ArrowRight");
    await expect(page.locator("media-fullscreen-button")).toBeFocused();

    // A menu keeps the arrows, and its button takes focus back.
    await rewind();
    await page.keyboard.press("ArrowLeft");
    await page.keyboard.press("Enter");
    await expect(page.locator("#subtitles-menu media-menu-radio-item").first()).toBeVisible();
    await page.keyboard.press("ArrowDown");
    expect(await focused(page)).not.toBe("Subtitles");
    await page.keyboard.press("Escape");
    await expect(page.getByRole("button", { name: "Subtitles" })).toBeFocused();
    await expect(page).toHaveURL(/\/demo\/watch\//);

    // With no key for a while, the bars go and focus with them.
    await rewind();
    await expect(bars).not.toHaveAttribute("data-held", /.*/, { timeout: 8_000 });
    await expect.poll(() => bars.evaluate((held) => getComputedStyle(held).opacity)).toBe("0");
    await rewind();
    await page.keyboard.press("ArrowRight");
    await expect.poll(() => media((held) => held.currentTime >= 9)).toBe(true);

    // A remote's back key, as an LG television reports it.
    await page.evaluate(() => {
      document.body.dispatchEvent(
        new KeyboardEvent("keydown", { keyCode: 461, bubbles: true, cancelable: true }),
      );
    });
    await expect(page).toHaveURL(THE_TITLE);
  });

  /** Opens the demo's title in the player, by pointing. */
  async function openPlayer(page: Page): Promise<void> {
    await page.goto(THE_TITLE);
    await page.getByRole("link", { name: /^(Resume|Play)/ }).click();
    await page.getByRole("button", { name: "Play now" }).click();
    await expect(page.locator("video")).toBeVisible();
  }

  test("keeps focus on Play now after the player was left by its Back link", async ({ page }) => {
    await openPlayer(page);
    await page.keyboard.press("ArrowUp");
    await page.keyboard.press("ArrowUp");
    await expect(page.getByRole("link", { name: "Back" })).toBeFocused();
    await page.keyboard.press("Enter");
    await expect(page).toHaveURL(THE_TITLE);

    const resume = page.getByRole("link", { name: /^(Resume|Play)/ });
    await resume.focus();
    await page.keyboard.press("Enter");
    const playNow = page.getByRole("button", { name: "Play now" });
    await expect(playNow).toBeVisible();
    // Long enough for focus to be restored, were it going to be.
    await page.waitForTimeout(200);
    await expect(playNow).toBeFocused();
  });

  test("moves focus on at the end of a video that ends with focus in the bars", async ({
    page,
  }) => {
    await openPlayer(page);
    await page.keyboard.press("ArrowUp");
    await expect(page.locator("media-controls")).toHaveAttribute("data-held", "true");
    await page.locator("video").evaluate((video: HTMLMediaElement) => {
      video.currentTime = video.duration - 0.2;
      void video.play();
    });
    await expect(page.getByRole("link", { name: "Play now" })).toBeVisible();
    await page.keyboard.press("ArrowRight");
    await expect(page.getByRole("button", { name: "Stay" })).toBeFocused();
  });

  test("goes back through the history from the player, not around in a loop", async ({ page }) => {
    await page.goto("/demo/library");
    await page.locator('main a[href*="/titles/"]').first().click();
    await expect(page).toHaveURL(/\/demo\/titles\//);
    await page.getByRole("link", { name: /^(Resume|Play)/ }).click();
    await page.getByRole("button", { name: "Play now" }).click();
    await expect(page.locator("video")).toBeVisible();

    await page.keyboard.press("Escape");
    await expect(page).toHaveURL(/\/demo\/titles\//);
    await page.goBack();
    await expect(page).toHaveURL("/demo/library");
  });

  test("goes back through the history from the player's Back link too", async ({ page }) => {
    await page.goto("/demo/library");
    await page.locator('main a[href*="/titles/"]').first().click();
    await expect(page).toHaveURL(/\/demo\/titles\//);
    await page.getByRole("link", { name: /^(Resume|Play)/ }).click();
    await page.getByRole("button", { name: "Play now" }).click();
    await expect(page.locator("video")).toBeVisible();
    await page.keyboard.press("ArrowUp");
    await page.keyboard.press("ArrowUp");
    await page.keyboard.press("Enter");
    await expect(page).toHaveURL(/\/demo\/titles\//);
    await page.goBack();
    await expect(page).toHaveURL("/demo/library");
  });

  test("goes to the title from a player started elsewhere", async ({ page }) => {
    await page.goto("/demo");
    await page.getByRole("link", { name: "Resume" }).first().click();
    await page.getByRole("button", { name: "Play now" }).click();
    await expect(page.locator("video")).toBeVisible();
    await page.keyboard.press("ArrowUp");
    await page.keyboard.press("ArrowUp");
    await page.keyboard.press("Enter");
    await expect(page).toHaveURL(/\/demo\/titles\//);
  });

  test("goes back through the history from a title's arrow too", async ({ page }) => {
    await page.goto("/demo");
    await page.getByRole("link", { name: "Library", exact: true }).first().click();
    await page.locator('main a[href*="/titles/"]').first().click();
    await expect(page).toHaveURL(/\/demo\/titles\//);
    await page.getByRole("link", { name: "Back" }).click();
    await expect(page).toHaveURL("/demo/library");
    await page.goBack();
    await expect(page).toHaveURL("/demo");
  });

  test("keeps where a list's filters left it when going back to it", async ({ page }) => {
    await page.goto("/demo/library");
    await page.getByRole("button", { name: "Movies" }).click();
    await expect(page).toHaveURL(/\?/);
    const filtered = page.url();
    await page.locator('main a[href*="/titles/"]').first().click();
    await expect(page).toHaveURL(/\/demo\/titles\//);
    await page.getByRole("link", { name: "Back" }).click();
    await expect(page).toHaveURL(filtered);
  });

  test("goes back once for a held key", async ({ page }) => {
    await page.goto("/demo/library");
    await page.locator('main a[href*="/titles/"]').first().click();
    await page.getByRole("link", { name: /^(Resume|Play)/ }).click();
    await page.getByRole("button", { name: "Play now" }).click();
    await expect(page.locator("video")).toBeVisible();
    await page.evaluate(() => {
      for (const repeat of [false, true, true]) {
        document.body.dispatchEvent(
          new KeyboardEvent("keydown", { key: "Escape", repeat, bubbles: true }),
        );
      }
    });
    await expect(page).toHaveURL(/\/demo\/titles\//);
    await page.waitForTimeout(200);
    await expect(page).toHaveURL(/\/demo\/titles\//);
  });

  test("returns to the same one of two links with one address", async ({ page }) => {
    await page.goto("/demo");
    const links = page.locator('main a[href$="trr_aaaaaaaaaaaaaaaaaaaaaaaaa1"]');
    await expect(links).toHaveCount(2);
    await links.last().focus();
    await page.keyboard.press("Enter");
    await expect(page).toHaveURL(/\/demo\/titles\//);
    await page.goBack();
    await expect(links.last()).toBeFocused();
  });

  test("does not run out of a text field on a held arrow", async ({ page }) => {
    await page.goto("/demo/settings/stremio");
    const field = page.locator('main input[type="password"]');
    await field.focus();
    await page.keyboard.type("abc");
    for (let press = 0; press < 6; press++) await page.keyboard.down("ArrowLeft");
    await page.keyboard.up("ArrowLeft");
    await expect(field).toBeFocused();
    await page.keyboard.press("ArrowLeft");
    await expect(page.getByRole("link", { name: "Stremio" })).toBeFocused();
  });

  test("goes down into a download panel stacked under a narrow title's actions", async ({
    page,
  }) => {
    await page.setViewportSize({ width: 800, height: 800 });
    await page.goto(THE_TITLE);
    await page.getByRole("link", { name: "Edit" }).focus();
    await page.keyboard.press("ArrowDown");
    await expect(page.getByRole("button", { name: /show all pieces/ })).toBeFocused();
  });

  test("leaves a video opened directly for its title", async ({ page }) => {
    await openPlayer(page);
    const url = page.url();
    await page.goto(url);
    await page.getByRole("button", { name: "Play now" }).click();
    await expect(page.locator("video")).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(page).toHaveURL(/\/demo\/titles\//);
  });

  test("leaves a track menu opened by pointing its arrow keys", async ({ page }) => {
    await openPlayer(page);
    await page.mouse.move(600, 300);
    await page.getByRole("button", { name: "Subtitles" }).click();
    const items = page.locator("#subtitles-menu media-menu-radio-item");
    await expect(items.first()).toBeFocused();
    await page.keyboard.press("ArrowDown");
    await expect(items.nth(1)).toBeFocused();
  });

  test("does not leave the player on Escape in fullscreen", async ({ page }) => {
    await openPlayer(page);
    await page.keyboard.press("f");
    await expect.poll(() => page.evaluate(() => document.fullscreenElement !== null)).toBe(true);
    // A real browser ends fullscreen itself, which a simulated key does not do; what is
    // tested is that the page does not take the key as well.
    await page.keyboard.press("Escape");
    await page.waitForTimeout(200);
    await expect(page).toHaveURL(/\/demo\/watch\//);
  });

  test("does not take a pointing viewer's focus, or a header link's", async ({ page }) => {
    await page.goto("/demo/library");
    await page.getByRole("link", { name: "Home", exact: true }).focus();
    await page.keyboard.press("Enter");
    await expect(page).toHaveURL("/demo");
    await page.getByRole("link", { name: "Library", exact: true }).first().focus();
    await page.keyboard.press("Enter");
    await expect(page).toHaveURL("/demo/library");
    await page.waitForTimeout(200);
    await expect(page.getByRole("link", { name: "TorrPlay" })).not.toBeFocused();

    // A card followed by pointing, and the list reached again by a link.
    await page.setViewportSize({ width: 1280, height: 400 });
    const card = page.locator('main a[href*="/titles/"]').last();
    await card.click();
    await expect(page).toHaveURL(/\/demo\/titles\//);
    await page.getByRole("link", { name: "Back" }).click();
    await expect(page).toHaveURL("/demo/library");
    await page.waitForTimeout(200);
    await expect(card).not.toBeFocused();
    await page.getByRole("link", { name: "Home", exact: true }).click();
    await page.getByRole("link", { name: "Library", exact: true }).first().click();
    await expect(page).toHaveURL("/demo/library");
    await page.waitForTimeout(200);
    await expect(card).not.toBeFocused();
    expect(await page.evaluate(() => window.scrollY)).toBe(0);
  });

  test("stays where the viewer scrolled on the first arrow", async ({ page }) => {
    await page.setViewportSize({ width: 1280, height: 600 });
    await page.goto(THE_TITLE);
    await expect(page.getByRole("link", { name: /^(Resume|Play)/ })).toBeVisible();
    await page.mouse.wheel(0, 500);
    await expect.poll(() => page.evaluate(() => window.scrollY)).toBeGreaterThan(300);
    const scrolled = await page.evaluate(() => window.scrollY);
    await page.keyboard.press("ArrowDown");
    await page.waitForTimeout(100);
    expect(await page.evaluate(() => window.scrollY)).toBeGreaterThanOrEqual(scrolled - 50);
    await expect(page.getByRole("link", { name: /^(Resume|Play)/ })).not.toBeFocused();
  });

  test("leaves the player with Escape", async ({ page }) => {
    await page.goto(THE_TITLE);
    await page.getByRole("link", { name: /^(Resume|Play)/ }).click();
    await page.getByRole("button", { name: "Play now" }).click();
    await expect(page.locator("video")).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(page).toHaveURL(THE_TITLE);
  });

  test("chooses a profile on a device's first start", async ({ page, request }) => {
    const first = "An invented viewer";
    const names = [first, "Another invented viewer"];
    const created: string[] = [];
    for (const name of names) {
      const response = await request.post("/api/v1/profiles", { data: { name } });
      expect(response.ok()).toBe(true);
      created.push(response.headers().location ?? "");
    }
    try {
      await page.goto("/");
      const heading = page.getByRole("heading", { level: 1 });
      await expect(heading).toBeVisible();
      await expect(page.getByRole("button", { name: first })).toBeVisible();
      await page.keyboard.press("ArrowDown");
      const buttons = page.locator("main li button");
      await expect(buttons.first()).toBeFocused();
      const count = await buttons.count();
      expect(count).toBeGreaterThanOrEqual(2);
      for (let index = 1; index < count; index++) {
        await page.keyboard.press("ArrowRight");
        await expect(buttons.nth(index)).toBeFocused();
      }
      await page.keyboard.press("ArrowLeft");
      await expect(buttons.nth(count - 2)).toBeFocused();
      await page.keyboard.press("Enter");
      await expect(page.locator("main li button")).toHaveCount(0);
    } finally {
      for (const location of created) {
        expect((await request.delete(location)).ok()).toBe(true);
      }
    }
  });

  test("signs in", async ({ page, request }) => {
    const PASSWORD = "an invented test passphrase";
    expect((await request.put("/api/v1/password", { data: { password: PASSWORD } })).ok()).toBe(
      true,
    );
    try {
      await page.goto("/");
      const field = page.getByLabel("Password");
      await expect(field).toBeFocused();
      await field.fill(PASSWORD);
      await page.keyboard.press("ArrowDown");
      await expect(page.getByRole("button", { name: "Sign in" })).toBeFocused();
      await page.keyboard.press("ArrowUp");
      await expect(field).toBeFocused();
      await page.keyboard.press("ArrowDown");
      await page.keyboard.press("Enter");
      await expect(page).toHaveURL("/");
      await expect(field).toHaveCount(0);
    } finally {
      const signedIn = await request.post("/api/v1/access-tokens", {
        data: { password: PASSWORD, name: "Cleanup", scope: "api" },
      });
      const { secret } = (await signedIn.json()) as { secret: string };
      const headers = { Authorization: `Bearer ${secret}` };
      expect((await request.delete("/api/v1/password", { headers })).ok()).toBe(true);
    }
  });
});
