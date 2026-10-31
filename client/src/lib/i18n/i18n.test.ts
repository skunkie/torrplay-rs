// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { expect, it } from "vitest";

import { profile } from "../../testing";
import { ApiError } from "../api/http";
import { en } from "./en";
import { detect, failureMessage, i18n, profileName } from "./index.svelte";
import { ru } from "./ru";

it.each([
  [["ru-RU", "en-US"], "ru"],
  [["en-GB", "ru"], "en-US"],
  [["de-DE", "ru"], "ru"],
  [["de-DE"], "en-US"],
  [[], "en-US"],
])("picks the language for the browser's %j", (languages, expected) => {
  expect(detect(languages)).toBe(expected);
});

function problem(type: string, detail?: string): ApiError {
  return new ApiError(403, {
    type: `tag:skunkie.github.io,2026:torrplay/problems/${type}`,
    title: "",
    status: 403,
    ...(detail ? { detail } : {}),
  });
}

it("tells a known problem in the current language and others as the server does", () => {
  i18n.locale = "ru";
  expect(failureMessage(problem("invalid-credentials", "Wrong."))).toBe("Неверный пароль.");
  expect(failureMessage(problem("duplicate-torrent", "Already added."))).toBe("Already added.");
  expect(failureMessage(new ApiError(502, undefined))).toBe("Запрос не выполнен, код ответа 502.");
  expect(failureMessage(new TypeError("fetch failed"))).toBe("Не удалось подключиться к TorrPlay.");
});

it("labels the profiles the server creates while they keep its names", () => {
  i18n.locale = "ru";
  expect(profileName(profile(1, "Devices", "device"))).toBe("Устройства");
  expect(profileName(profile(2, "Default"))).toBe("Основной");
  expect(profileName(profile(3, "Ana"))).toBe("Ana");
});

/** Every message of `messages` as text, functions called with sample
 * arguments: a count, or a name that is clearly not Russian. */
function texts(messages: object, path = ""): [string, string][] {
  return Object.entries(messages).flatMap(([key, value]): [string, string][] => {
    const at = `${path}${key}`;
    if (typeof value === "string") return [[at, value]];
    if (typeof value === "function") {
      const args = Array.from({ length: value.length }, () => 7);
      return [[at, String(value(...args))]];
    }
    return texts(value as object, `${at}.`);
  });
}

it("has every message in Russian, in Russian typography", () => {
  for (const [at, text] of texts(ru)) {
    const words = text.replaceAll("TorrPlay", "");
    expect(words, at).not.toMatch(/[A-Za-z]/);
    expect(text, at).not.toMatch(/["']/);
    expect(text, at).not.toContain("...");
    expect(text, at).not.toMatch(/\d [а-я]/);
  }
});

it("gives each English message a Russian one", () => {
  expect(texts(ru).map(([at]) => at)).toEqual(texts(en).map(([at]) => at));
});
