// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { render, screen } from "@testing-library/svelte";
import { expect, it } from "vitest";

import NotFound from "./NotFound.svelte";

it("names the missing path and links home", () => {
  render(NotFound, { path: "/nowhere" });
  expect(screen.getByRole("heading")).toHaveTextContent("Page not found");
  expect(screen.getByText(/\/nowhere/)).toBeInTheDocument();
  expect(screen.getByRole("link", { name: "Go home" })).toHaveAttribute("href", "/");
});
