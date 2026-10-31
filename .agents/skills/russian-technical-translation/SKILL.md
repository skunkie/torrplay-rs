---
name: russian-technical-translation
description: Guidelines for writing and reviewing the web client's Russian UI strings so they read as natural, idiomatic Russian rather than a translation from English. Use this skill whenever adding, changing, translating, or reviewing a Russian message of the client, or an English message that has a Russian counterpart.
---

<!--
SPDX-FileCopyrightText: 2026 TorrPlay

SPDX-License-Identifier: MIT
-->

# Russian Technical Translation

This skill covers the client's user interface in Russian. The client ships in
American English and Russian; English is the source, and every message exists
in both. The project's documentation stays in English only.

## Core Objective

Russian UI copy must read as if a Russian-speaking engineer and interface
writer had written it first. It must **never** feel like a word-for-word
translation (calque) from English. Users read it on phones, desktops, and
televisions from several meters away, so it must also be short.

---

## 1. Anti-Patterns of Mechanical Translation

### A. Copula Verbs and the Passive («является», «быть»)

- **Mechanical:** `TorrPlay является приложением для просмотра торрентов.`
- **Natural:** `TorrPlay — приложение для просмотра торрентов.`
- **Mechanical:** `Этот токен может быть использован внешними плеерами.`
- **Natural:** `Токен подходит для внешних плееров.`

### B. Chains of Genitives (нанизывание родительного падежа)

- **Mechanical:** `Ошибка процесса получения метаданных торрента пользователя`
- **Natural:** `Не удалось получить метаданные торрента`

### C. Bureaucratic and Wordy Calques (канцелярит и буквализм)

| Mechanical | Natural | Rationale |
| :--- | :--- | :--- |
| в целях обеспечения | чтобы / для | Bureaucratic filler |
| при осуществлении запуска | при запуске | Split verb with «осуществление» |
| данный / соответствующий | этот / нужный / omit | Needless demonstrative |
| в экземпляре включена | в TorrPlay включена | «Экземпляр» is a clunky calque of "instance" |
| позволяет вам выполнять | позволяет выполнять / можно | Literal "allows you to" |
| предоставляет возможность | умеет / поддерживает / позволяет | Pompous |
| под капотом | внутри / name the component | Idiom calque |
| бесшовно | плавно / без перерывов / сразу | Corporate buzzword |
| обогащение | добавление / загрузка / обновление | Calque of "enrich" |
| прямо из / прямо в | из / в / omit | Emphatic "directly" calque |
| конечная точка | эндпоинт / маршрут API | Literal "endpoint" |
| нативная поддержка | встроенная поддержка | "Native support" of a browser or platform |
| ридеры | потоки чтения / воспроизведение | Calque of stream "readers" |
| успешно сохранено | Сохранено | "Successfully" adds nothing |
| Пожалуйста, введите | Введите | Courtesy words lengthen UI copy |

### D. Clunky Prepositional Phrases

- **Mechanical:** `В случае если вы забыли пароль…`
- **Natural:** `Если вы забыли пароль…`

### E. Pronouns and Lowercase «вы»

- **Address the user as «вы», never «ты»**, in every message.
- **Never capitalize «Вы» / «Ваш»** mid-sentence; that capital belongs to
  personal letters and marketing. Write `вы`, `вас`, `вам`, `ваш`,
  capitalized only at the start of a sentence.
- **Omit pronouns that add nothing:** `Укажите ваше имя и ваш пароль` →
  `Укажите имя и пароль`.

### F. Developer Slang

Keep a professional tone: `бинарник` → `исполняемый файл`, `конфа` →
`настройки`, `фича` → `возможность`, `деплоить` → `развёртывать`,
`залить` → `загрузить`, `скачка` → `загрузка`.

---

## 2. Terminology

Each concept has one Russian name everywhere in the client, as each has one
English name in [the glossary](../../../docs/glossary.md). Add a term here
before using it in a message, and use the glossary's English term in the
English message.

### Library and Playback

| Glossary term | Russian | Notes |
| :--- | :--- | :--- |
| Library | Библиотека | The page and the persisted torrents in it |
| Title | Фильм / Сериал when the kind is known; otherwise name the torrent | Never `тайтл` or `титул`, and not `фильм или сериал`, which is clumsy and wrong for other kinds: `Торрент не найден`, `Ничего не найдено`; «название» for a title's name |
| Kind: movie / series / other | Фильм / Сериал / Другое | Chips and filters |
| Episode | Серия | Not `эпизод`, which reads as a scene. Pages write `1 сезон, 2 серия`; cards and other short places write `С1 Е2` with Cyrillic letters, never `S1 E2` |
| Season | Сезон | Season 0 is `Спецвыпуски` |
| Category | Категория | |
| Genre | Жанр | |
| Poster | Постер | |
| Up next | Продолжить просмотр | The row and its entries; resuming and next episodes both belong to it |
| Viewed | Просмотрено | Status of a file |
| Next episode | Следующая серия | |
| Progress | Прогресс просмотра; often omitted | `Осталось 26 мин` says it without the noun |
| Dismissal | Убрать из раздела «Продолжить просмотр» | The action; the noun is rarely shown |
| Profile | Профиль | The chooser's heading is `Выберите профиль`; not `Кто смотрит?`, a calque, nor `Кто будет смотреть?` |
| Device profile | Устройства | The client's label for it; never offered as someone to watch as |
| Recently added | Недавно добавленные | |
| Search | Поиск | `Найти` for the action |
| Player | Плеер | Inanimate: `выберите плеер` |

### Torrents and Storage

| Glossary term | Russian | Notes |
| :--- | :--- | :--- |
| Torrent | Торрент | |
| Torrent file / metainfo | Торрент-файл | `metainfo` stays in code only |
| Magnet URI | Magnet-ссылка | |
| Info hash | Инфо-хеш | Dictionary spelling is `хеш`, not `хэш` |
| Metadata | Метаданные | |
| Metadata retrieval | Поиск файлов / получение метаданных | Users see `Поиск файлов…` |
| Persisted torrent | Торрент в библиотеке | |
| Temporary torrent | Временный торрент | |
| Storage mode: memory / disk | Оперативная память / Диск | Never bare `RAM` or `ОЗУ` |
| Preload | Предзагрузка | |
| Piece cache / cache | Кеш | Dictionary spelling, like `хеш`; never `кэш` |
| Peer / seed | Пир / сид | Standard BitTorrent usage |
| Background download | Фоновая загрузка | |

### TorrPlay as a Subject

`TorrPlay` is masculine, as `сервер` is: `TorrPlay недоступен`,
`TorrPlay запущен`, never `недоступно`.

### Engine

Do not translate **engine** as `движок`. Name what the user experiences: while
the application starts, restores, or stops, the banner says
`TorrPlay запускается`, `TorrPlay восстанавливает торренты`, or
`TorrPlay останавливается`; for the BitTorrent part, say `торрент-клиент`.

### Security

| Glossary term | Russian | Notes |
| :--- | :--- | :--- |
| Authentication | Аутентификация | Proving who is calling: the password or a token |
| Authorization | Авторизация | What a token's scope allows; never use it for signing in |
| Password | Пароль | |
| Access token | Токен доступа | |
| Playback token | Токен воспроизведения | |
| Sign in / sign out | Войти / Выйти | The page is `Вход` |

### Animacy of Borrowed Nouns

Nouns that name software (`плеер`, `клиент`, `сервер`, `профиль`) are
inanimate: the accusative matches the nominative. Write `выберите плеер`,
not `выберите плеера`. Genitives are unaffected: `для внешних плееров`.

---

## 3. UI Copy

### A. Brevity

- **Buttons and menu items** use an infinitive or a short noun phrase:
  `Войти`, `Сохранить`, `Удалить`, `Показать ещё`, `Повторить`.
- **Labels and badges** fit one line, about 30 characters: `Поиск файлов`,
  `Приостановлен`, `Ошибка`.
- **Russian runs about 30% longer than English.** Layouts must let labels
  wrap; never shorten a Russian message into ambiguity to fit a width
  chosen for English. Check each changed page in Russian at 360 pixels wide.
- **Sentence case** in both languages: `Недавно добавленные`, never
  `Недавно Добавленные`.

### B. Mood

- Buttons and setting names: infinitive or noun phrase.
- Instructions in errors and empty states: imperative with `вы` implied
  (`Введите пароль`, `Попробуйте ещё раз`).
- Questions that confirm an action mirror the button:
  `Удалить «Night Ferry»?` with `Удалить` / `Отмена`.

### C. Capitalization After a Colon or Arrow

Text that continues the sentence after a colon or `→` stays lowercase:
`Ошибка: торрент не найден`. A label or value inserted after a colon keeps
its own capital: `Тема: Светлая`, `Режим хранения: Оперативная память`.

### D. Setting Values Answer the Name

A value shown under or beside a setting must not repeat the setting's name:
`Тема` → `Как в системе` / `Светлая` / `Тёмная`, not `Светлая тема`.

### E. Templates and Placeholders

Messages are templates; `{name}`, `{count}`, and similar parameters are
filled at runtime, and the text around them must stay grammatical for every
value.

- **Check what fills a parameter** before choosing the grammar around it. A
  parameter filled with library data (a title, a genre, a profile name) or
  another translated label arrives in the nominative and cannot be inflected.
  - *Broken:* `Профиль переключён на {name}` → `…на Аня`.
  - *Fixed:* `Профиль: {name}`, or put the name where the nominative fits.
- **Quote library data** shown inside a sentence with «ёлочки»:
  `Убрать «{title}» из раздела «Продолжить просмотр»`.
- **Agree with the enclosing message**, not with the English: a fragment
  shown inside another string takes that string's gender and number.

### F. Plurals

Russian has three plural forms (`1 серия`, `2 серии`, `5 серий`, and
`21 серия`), which `Intl.PluralRules("ru")` names `one`, `few`, and `many`;
fractions use `other`.

- Every message with a count gives all forms Russian needs, through the
  message library's plural variants. Never build a plural by appending a
  suffix in code, and never reuse the English singular/plural split.
- A unit abbreviation needs no plural: `Осталось 26 мин`, `5 ч 10 мин`.
  Prefer it where space is short.

### G. Numbers, Dates, and Sizes

- Format numbers, dates, times, durations, and relative times with `Intl`
  for the current locale; never assemble them from strings. Russian uses a
  decimal comma and a space between thousands, which `Intl` provides.
- Put a non-breaking space between a number and its unit: `100 мс`,
  `256 МиБ`.
- **Sizes are binary**, with IEC symbols, in both languages: `B`, `KiB`,
  `MiB`, `GiB`, `TiB` and `Б`, `КиБ`, `МиБ`, `ГиБ`, `ТиБ`, with
  1 MiB = 1,048,576 bytes. They match the settings' values (`256 МиБ`), the
  documentation, and what torrent clients such as qBittorrent show. Never
  show decimal units (`MB`, `МБ`), and never label a binary value `MB`.
- **Rates** are binary bytes per second, like sizes: `12,5 МиБ/с`,
  `12.5 MiB/s`, never bits (`Мбит/с`).
- `Intl` has no binary units, so sizes and rates go through the client's
  formatting helper: it formats the number with `Intl.NumberFormat` for the
  locale, chooses the unit, and joins the localized symbol with a
  non-breaking space. Do not use `Intl`'s byte or bit units; its Russian
  rates also end in a Latin `c` (`Мбит/c`).
- Grep for units and labels built in code (such as `` `${n} min left` ``)
  that bypass the messages, and move them into messages.

### H. Errors

- The server's Problem Details `detail` is English. Show the client's own
  message for each problem type it knows (`Неверный пароль.`,
  `Слишком много попыток. Подождите немного.`), and the server's text only
  for a type the client does not know.
- Say what happened and what to do, without blame or apology:
  `Не удалось подключиться к TorrPlay. Попробуйте ещё раз.`

### I. Library Data Stays as Stored

Titles, descriptions, genres, categories, file names, and the names people
give profiles are data, shown exactly as stored. Do not translate,
transliterate, or re-capitalize them.

The two profiles the server creates are the exception: the device profile,
named `Devices`, and the first person profile, named `Default` until someone
renames it. The client shows its own label for them, `Устройства` for the
device profile (recognized by its kind) and `Основной` for a person profile
whose name is still exactly `Default`.

### J. Accessibility Text

- `aria-label`, `title`, image `alt`, and text shown only to screen readers
  are messages like any other, in both languages, and follow every rule
  here: `Убрать «{title}» из раздела «Продолжить просмотр»`.
- The page's `<html lang>` follows the chosen language, `en-US` or `ru`, so
  screen readers pronounce the text correctly.

---

## 4. Typography

- **Letter «ё».** Write «ё» wherever it belongs, including short participles
  and adjectives (`включён`, `сохранён`, `тёмная`) and words like `ещё`,
  `её`, `объём`, `учётный`. Watch pairs it distinguishes, such as `все` /
  `всё`.
- **Quotation marks.** Angle quotes («ёлочки») in visible text, „лапки“
  inside them. Straight quotes only in code.
- **Dashes.** A spaced em dash (`—`) for definitions and asides; a hyphen
  without spaces in compounds (`веб-интерфейс`, `magnet-ссылка`,
  `торрент-файл`). Ranges use an en dash without spaces: `1–3`.
- **Ellipsis.** One character (`…`) for progress states: `Загрузка…`,
  `Поиск файлов…`.
- **Non-breaking spaces** between a number and its unit, and after short
  words that must not end a line (`в`, `и`, `с`, `к`, `о`) in longer
  messages. Write them as `\u00A0` in the messages file, never as the
  invisible character, so a reviewer can see them.

---

## 5. Accuracy Before Wording

A faithful translation of a wrong English message is still wrong.

- **Describe what the code does.** Check the behavior in the client and the
  server (`crates/`, `api/openapi.yaml`, `docs/lifecycle.md`) before writing
  either language.
- **Cover everything a setting controls**, not only its most visible effect.
- **Fit every place a message is shown.** Grep the message's key; when one
  key is shown in several places, its text must be true in all of them.
- **Fix the English first.** When the English message uses jargon or states
  something inaccurate, correct it, then translate the corrected text, in
  the same change.

---

## 6. What Is Never Translated

- Code, paths, query parameters, JSON members, and problem type names.
- Protocol and format names: BitTorrent, DLNA, UPnP, Stremio, MCP, HTTP,
  Matroska, WebVTT.
- `TorrPlay` and other product names.
- Episode codes inside file names, such as `S01E02`; the client's own short form is `С1 Е2`.

---

## 7. Self-Review Checklist

When adding or changing a Russian message:

1. **Read aloud:** does it flow as Russian, or as English with Russian words?
2. **Wordiness:** no `является`, `данный`, `осуществление`, `в целях`,
   `успешно`, `пожалуйста`.
3. **Pronouns:** the user addressed as `вы`, lowercase; needless pronouns
   removed.
4. **Brevity:** buttons and labels short enough for a television; the page
   still works at 360 pixels in Russian.
5. **Terminology:** every term from the tables above, one term per concept,
   and borrowed software nouns inanimate.
6. **Templates:** each parameter's source checked; the text grammatical for
   every value; library data quoted and uninflected.
7. **Plurals:** every count has `one`, `few`, `many`, and `other` forms, or
   uses an abbreviated unit.
8. **Formatting:** numbers, dates, and durations come from `Intl`; units
   separated by a non-breaking space.
9. **Typography:** «ё» everywhere it belongs, «ёлочки», correct dashes,
   one-character ellipsis, `\u00A0` where a space must not break, and
   capitals after a colon only for inserted labels and values.
10. **Accessibility:** labels, titles, and screen-reader text translated
    like visible text.
11. **Accuracy:** the message matches what the client and server do, at every
    place its key is shown, and the English says the same thing.
12. **Parity:** the message exists in both languages, and the client's checks
    and tests pass.
