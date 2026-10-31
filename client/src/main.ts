// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import "./app.css";

import { mount } from "svelte";

import App from "./App.svelte";

const target = document.getElementById("app");
if (!target) {
  throw new Error("index.html has no #app element");
}
mount(App, { target });
