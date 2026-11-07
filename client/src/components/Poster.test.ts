// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { render } from "@testing-library/svelte";
import { expect, it } from "vitest";

import Poster from "./Poster.svelte";

it.each([
  ["a picture", "/poster.jpg"],
  ["no picture", undefined],
])("lets a long name break between its parts with %s", (_, url) => {
  const { container } = render(Poster, { url, title: "The.Invented.Name" });

  expect(container.querySelectorAll("wbr")).toHaveLength(3);
});

it.each([
  ["a picture", "/poster.jpg"],
  ["no picture", undefined],
])("keeps an abbreviation whole in a title with spaces with %s", (_, url) => {
  const { container } = render(Poster, { url, title: "Keepers of the L.I.G.H.T. House" });

  expect(container.querySelectorAll("wbr")).toHaveLength(1);
});
