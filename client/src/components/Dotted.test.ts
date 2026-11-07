// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { render } from "@testing-library/svelte";
import { expect, it } from "vitest";

import Dotted from "./Dotted.svelte";

it("reads as its parts with a dot between them, leaving out empty ones", () => {
  const { container } = render(Dotted, { parts: ["One", undefined, "", null, "Two"] });

  expect(container.textContent?.trim()).toBe("One · Two");
  expect(container.querySelectorAll(".dotted")).toHaveLength(2);
});

it("shows nothing without parts", () => {
  const { container } = render(Dotted, { parts: [] });

  expect(container.querySelectorAll(".dotted")).toHaveLength(0);
  expect(container.textContent?.trim()).toBe("");
});
