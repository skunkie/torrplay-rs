// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import "@testing-library/jest-dom/vitest";

import { beforeEach } from "vitest";

import { i18n } from "./lib/i18n/index.svelte";

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
