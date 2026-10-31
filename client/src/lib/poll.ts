// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

/** Runs `task` now and again `intervalMs` after each run ends, while the
 * page is visible: a hidden page polls nothing, and showing it again runs
 * the task at once. Runs never overlap. Returns what stops it. */
export function poll(
  task: () => Promise<void>,
  intervalMs: number,
  page: Pick<Document, "visibilityState" | "addEventListener" | "removeEventListener"> = document,
): () => void {
  let timer: ReturnType<typeof setTimeout> | undefined;
  let running = false;
  let stopped = false;
  // A function, so each check reads the visibility anew after an await.
  const hidden = () => page.visibilityState === "hidden";

  const run = async () => {
    clearTimeout(timer);
    timer = undefined;
    if (stopped || running || hidden()) {
      return;
    }
    running = true;
    try {
      await task();
    } catch {
      // The task reports its own failures; the next run tries again.
    } finally {
      running = false;
    }
    if (!stopped && !hidden()) {
      timer = setTimeout(run, intervalMs);
    }
  };
  const visibility = () => {
    if (hidden()) {
      clearTimeout(timer);
      timer = undefined;
    } else {
      void run();
    }
  };
  page.addEventListener("visibilitychange", visibility);
  void run();
  return () => {
    stopped = true;
    clearTimeout(timer);
    page.removeEventListener("visibilitychange", visibility);
  };
}
