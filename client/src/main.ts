// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import "@fontsource-variable/onest/wght.css";
import "@fontsource/jetbrains-mono/500.css";
import "./app.css";

import { mount } from "svelte";

import App from "./App.svelte";

const target = document.getElementById("app");
if (!target) {
  throw new Error("index.html has no #app element");
}
mount(App, { target });
