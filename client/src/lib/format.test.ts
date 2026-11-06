// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { expect, it } from "vitest";

import { formatBytes, formatFromNow } from "./format";
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

const NOW = Date.parse("2026-11-06T12:00:00Z");

it.each([
  ["2026-11-06T11:50:00Z", "this hour"],
  ["2026-11-06T09:00:00Z", "3 hours ago"],
  ["2026-11-04T12:00:00Z", "2 days ago"],
  ["2026-11-09T12:00:00Z", "in 3 days"],
  ["2026-08-08T12:00:00Z", "3 months ago"],
  ["2029-11-06T12:00:00Z", "in 3 years"],
])("tells %s as %s", (at, told) => {
  expect(formatFromNow(at, NOW)).toBe(told);
});
