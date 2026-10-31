// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import "@testing-library/jest-dom/vitest";

import { beforeEach } from "vitest";

import { i18n } from "./lib/i18n/index.svelte";

// Tests read the English messages unless they choose another language.
beforeEach(() => {
  i18n.locale = "en-US";
});
