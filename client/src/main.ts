// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import "@fontsource-variable/onest/wght.css";
import "@fontsource/jetbrains-mono/500.css";
import "./app.css";

import { mount } from "svelte";

import App from "./App.svelte";
import { t } from "./lib/i18n/index.svelte";
import { demo } from "./lib/routes";

const target = document.getElementById("app");
if (!target) {
  throw new Error("index.html has no #app element");
}
if (demo) {
  // Only the demo loads this, so nothing else waits for it or needs it.
  import("./lib/demo/backend").then(
    ({ demoBackend }) => mount(App, { target, props: { backend: demoBackend() } }),
    () => {
      target.textContent = t().demo.failed;
    },
  );
} else {
  mount(App, { target });
}
