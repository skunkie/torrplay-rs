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

    await expect(page.getByText(/^Version .+ · .+ .+$/)).toBeVisible();
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

  test("shows posters it serves itself, and a tile for a torrent with none", async ({ page }) => {
    await page.goto("/demo/library");
    const poster = page.locator('img[src="/demo-media/posters/paper-moons.webp"]').first();
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
