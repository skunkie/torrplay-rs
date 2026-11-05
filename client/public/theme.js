// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

// Runs before the first paint, so the page never flashes the other theme. It
// is a file because the content security policy refuses inline scripts, and
// plain because a module would run too late. lib/theme.svelte.ts takes over
// once the client starts, and theme.test.ts keeps the two alike.
{
  let choice = null;
  try {
    choice = localStorage.getItem("torrplay.theme");
  } catch {
    // Private windows may refuse storage; the system decides.
  }
  const light =
    choice === "light" ||
    (choice !== "dark" && matchMedia("(prefers-color-scheme: light)").matches);
  document.documentElement.setAttribute("data-theme", light ? "light" : "dark");
  document
    .querySelector('meta[name="color-scheme"]')
    .setAttribute("content", light ? "light" : "dark");
  document
    .querySelector('meta[name="theme-color"]')
    .setAttribute("content", light ? "#f8fafc" : "#0f172a");
}
