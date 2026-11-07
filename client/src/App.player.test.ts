// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { render, screen } from "@testing-library/svelte";
import { expect, it, vi } from "vitest";

import App from "./App.svelte";
import { navigate } from "./lib/navigation.svelte";
import { watchPath } from "./lib/routes";
import { fakeBackend } from "./testing";

// As after an upgrade, when an open page asks for what is no longer there.
vi.mock("./pages/WatchPage.svelte", () => {
  throw new Error("the chunk is gone");
});

it("says so when the player cannot load, with a way out", async () => {
  navigate(watchPath("trr_1", "fil_1"));
  render(App, { backend: fakeBackend() });

  expect(await screen.findByRole("alert")).toHaveTextContent("The player could not be loaded.");
  expect(screen.getByRole("button", { name: "Try again" })).toBeInTheDocument();
  expect(screen.getByRole("link", { name: "Go home" })).toBeInTheDocument();
});
