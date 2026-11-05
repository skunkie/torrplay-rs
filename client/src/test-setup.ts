// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import "@testing-library/jest-dom/vitest";

import { beforeEach, vi } from "vitest";

import { i18n } from "./lib/i18n/index.svelte";

// jsdom has no layout, which Svelte's width bindings observe.
globalThis.ResizeObserver = class {
  observe = vi.fn();
  unobserve = vi.fn();
  disconnect = vi.fn();
};

// jsdom has no media queries; this system prefers neither theme.
window.matchMedia = ((query: string) => ({
  matches: false,
  media: query,
  addEventListener: vi.fn(),
  removeEventListener: vi.fn(),
})) as never;

// jsdom has no canvas, and complains when asked for one.
HTMLCanvasElement.prototype.getContext = (() => null) as never;

// jsdom does not implement modal dialogs.
HTMLDialogElement.prototype.showModal = function showModal(this: HTMLDialogElement) {
  this.open = true;
};
HTMLDialogElement.prototype.close = function close(this: HTMLDialogElement) {
  this.open = false;
  this.dispatchEvent(new Event("close"));
};

// Tests read the English messages unless they choose another language.
beforeEach(() => {
  i18n.locale = "en-US";
});
