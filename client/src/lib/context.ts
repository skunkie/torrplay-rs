// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { getContext, setContext } from "svelte";

import type { Backend } from "./backend";

const BACKEND = Symbol("backend");

/** Gives the pages below `backend`, the API's or a test's. */
export function provideBackend(backend: Backend): void {
  setContext(BACKEND, backend);
}

/** The backend the app provides. */
export function useBackend(): Backend {
  const backend = getContext<Backend | undefined>(BACKEND);
  if (!backend) {
    throw new Error("no backend is provided");
  }
  return backend;
}

/** The context that provides `backend`, for rendering a page in tests. */
export function backendContext(backend: Backend): Map<symbol, Backend> {
  return new Map([[BACKEND, backend]]);
}
