// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { expect, it } from "vitest";

import { formatBytes } from "./format";
import { i18n } from "./i18n/index.svelte";

it.each([
  [0, "0\u00A0B"],
  [0.34, "0.3\u00A0B"],
  [1023, "1,023\u00A0B"],
  [1024, "1\u00A0KiB"],
  [13.5 * 1024 ** 2, "13.5\u00A0MiB"],
  [32 * 1024 ** 2, "32\u00A0MiB"],
  [1024 ** 5, "1,024\u00A0TiB"],
])("writes %d bytes in binary units", (bytes, written) => {
  expect(formatBytes(bytes)).toBe(written);
});

it("follows the page's language", () => {
  i18n.locale = "ru";
  expect(formatBytes(13.5 * 1024 ** 2)).toBe("13,5\u00A0МиБ");
});
