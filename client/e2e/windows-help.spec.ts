// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { expect, test } from "@playwright/test";
import { readFileSync } from "node:fs";

const script = readFileSync(new URL("../../desktop/src/help-shortcut.js", import.meta.url), "utf8");
const origin = "http://127.0.0.1:18082";
const help = `${origin}/help-request`;

test("F1 requests desktop help once without leaving the current page", async ({ page }) => {
  const requests: string[] = [];
  await page.addInitScript({
    content: `(() => {\n${script}\ninstallHelpShortcut(${JSON.stringify(help)});\n})();`,
  });
  await page.route("**/*", async (route) => {
    const url = route.request().url();
    if (url === help) {
      requests.push(url);
      // The desktop cancels this navigation and opens the local file separately.
      await route.abort("aborted");
    } else {
      const body = url.endsWith("/library") ? '<input><iframe src="/frame"></iframe>' : "<input>";
      await route.fulfill({ contentType: "text/html", body });
    }
  });

  await page.goto(`${origin}/library`);
  await page.locator("input").press("Shift+F1");
  await page.evaluate(() => window.dispatchEvent(new KeyboardEvent("keydown", { key: "F1" })));
  await page.frameLocator("iframe").locator("input").press("F1");
  expect(requests).toEqual([]);

  await page.locator("input").focus();
  const requested = page.waitForRequest(
    (request) => request.url() === help && request.frame() === page.mainFrame(),
  );
  await page.keyboard.down("F1");
  await page.keyboard.down("F1");
  await page.keyboard.up("F1");
  await requested;
  await expect.poll(() => requests).toEqual([help]);
  await expect(page).toHaveURL(`${origin}/library`);

  await page.goto("http://127.0.0.1:18083/library");
  await page.locator("input").press("F1");
  expect(requests).toEqual([help]);
});
