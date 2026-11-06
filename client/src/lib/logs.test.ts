// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { expect, it } from "vitest";

import { logRecord, logRecordPage } from "../testing";
import { LOG_KEPT, withNewer } from "./logs";

function kept(count: number) {
  return Array.from({ length: count }, (_, index) => logRecord(count - index));
}

it("puts newer records above those kept, up to the limit", () => {
  const merged = withNewer(kept(LOG_KEPT - 1), logRecordPage([logRecord(LOG_KEPT)]));
  expect(merged).toHaveLength(LOG_KEPT);
  expect(merged?.[0]?.sequence).toBe(LOG_KEPT);
  expect(merged?.[1]?.sequence).toBe(LOG_KEPT - 1);
});

it("asks to read again rather than keep too many records", () => {
  expect(withNewer(kept(LOG_KEPT), logRecordPage([logRecord(LOG_KEPT + 1)]))).toBeUndefined();
});

it("asks to read again rather than leave a gap", () => {
  const page = logRecordPage([logRecord(300)], "/api/v1/log-records?cursor=gap");
  expect(withNewer(kept(1), page)).toBeUndefined();
});
