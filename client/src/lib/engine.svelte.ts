// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { ApiError } from "./api/http";
import type { Backend } from "./backend";
import { failureMessage } from "./i18n/index.svelte";

/** Whether the application can serve what needs its engine, which only tells
 * ready from not ready: starting and stopping look alike. */
export const engine = $state<{ status: "ready" | "unavailable" | "unreachable" }>({
  status: "ready",
});

/** Reads the application's readiness into {@link engine}. */
export async function checkEngine(backend: Backend): Promise<void> {
  try {
    engine.status = (await backend.getReadiness()) ? "ready" : "unavailable";
  } catch {
    engine.status = "unreachable";
  }
}

/** What a page that polls should show for `failure`, or `undefined` for one
 * the banner already reports, which the page's next read retries. */
export function pollFailure(failure: unknown): string | undefined {
  // Only while the banner says the same: a read can fail by itself.
  if (!(failure instanceof ApiError)) {
    return engine.status === "unreachable" ? undefined : failureMessage(failure);
  }
  if (failure.type === "engine-unavailable") {
    engine.status = "unavailable";
    return undefined;
  }
  return failureMessage(failure);
}
