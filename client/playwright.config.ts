// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { defineConfig } from "@playwright/test";

import { APPLICATION_URL } from "./e2e/constants";

export default defineConfig({
  testDir: "e2e",
  globalSetup: "./e2e/rig.ts",
  // The tests share one application and its library.
  workers: 1,
  forbidOnly: true,
  reporter: "list",
  timeout: 60_000,
  use: {
    baseURL: APPLICATION_URL,
    // The installed browser, so nothing is downloaded to run these.
    channel: "chrome",
    launchOptions: { args: ["--autoplay-policy=no-user-gesture-required"] },
    trace: "retain-on-failure",
  },
});
