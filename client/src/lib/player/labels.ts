// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

// The player's own labels follow the page's language through its i18n
// provider. Russian comes from Video.js's pack, registered with the bundle
// so it shows on first paint, with the labels the Russian translation
// skill words differently replaced: «ё», a non-breaking space before a
// unit, and errors that do not address the user. The pack is imported
// first so its other labels stay.
import "@videojs/html/i18n/locales/ru/register";

import { registerI18n } from "@videojs/html/i18n";

registerI18n("ru", {
  seek: {
    forward: "Перемотать вперёд на {seconds}\u00A0с",
    backward: "Перемотать назад на {seconds}\u00A0с",
  },
  errors: {
    aborted: "Воспроизведение остановлено до конца файла.",
    decode:
      "Не удалось воспроизвести этот медиафайл. Возможно, он повреждён или браузер не поддерживает его формат.",
    source:
      "Не удалось загрузить этот медиафайл. Возможно, он недоступен или браузер не поддерживает его формат.",
  },
});
