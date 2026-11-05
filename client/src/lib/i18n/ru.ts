// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import type { Messages } from "./en";
import { plural } from "./plural";

/** The client's messages in Russian, written by the Russian translation
 * skill in `.agents/skills/russian-technical-translation`. */
export const ru: Messages = {
  common: {
    loading: "Загрузка…",
    retry: "Повторить",
    unreachable: "Не удалось подключиться к TorrPlay.",
    failed: (status: number) => `Запрос не выполнен, код ответа ${status}.`,
    untitled: "Без названия",
  },
  units: ["Б", "КиБ", "МиБ", "ГиБ", "ТиБ"],
  nav: {
    main: "Основная навигация",
    home: "Главная",
    menu: "Меню",
    library: "Библиотека",
    profile: "Профиль",
    language: "Язык",
    signOut: "Выйти",
  },
  kinds: { movie: "Фильм", series: "Сериал", other: "Другое" },
  states: { retrieving_metadata: "Поиск файлов", paused: "Приостановлен", failed: "Ошибка" },
  progress: {
    viewed: "Просмотрено",
    resume: "Продолжить",
    nextEpisode: "Следующая серия",
    minutesLeft: (minutes: number) => `Осталось ${minutes}\u00A0мин`,
  },
  episodes: {
    short: (season: number, episode: number) => `С${season}\u00A0Е${episode}`,
    episode: (episode: number) => `${episode}\u00A0серия`,
    season: (season: number) => `${season}\u00A0сезон`,
    specials: "Спецвыпуски",
    long: (season: number, episode: number) => `${season}\u00A0сезон, ${episode}\u00A0серия`,
  },
  player: {
    back: "Назад",
    fileNotFound: "В торренте нет такого файла.",
    finished: "Просмотр завершён",
    playNext: "Смотреть",
    playNow: "Смотреть сейчас",
    preloading: "Предзагрузка",
    preloadQueued: "Ждёт другие предзагрузки",
    stay: "Остаться",
    nextIn: (seconds: number) =>
      `Начнётся через ${seconds}\u00A0${plural("ru", seconds, { one: "секунду", few: "секунды", many: "секунд", other: "секунды" })}`,
  },
  audio: {
    track: "Звуковая дорожка",
    short: "Звук",
    mono: "Моно",
    stereo: "Стерео",
    channels: (count: number) =>
      `${count}\u00A0${plural("ru", count, { one: "канал", few: "канала", many: "каналов", other: "канала" })}`,
    unknownLanguage: "Язык не указан",
    failed: "Не удалось воспроизвести эту звуковую дорожку.",
  },
  subtitles: {
    track: "Субтитры",
    off: "Выкл.",
    forced: "форсированные",
    sdh: "для слабослышащих",
    commentary: "комментарии",
    unsupported: "не поддерживается",
    failed: "Не удалось загрузить субтитры.",
  },
  home: {
    upNext: "Продолжить просмотр",
    upNextEmpty: "Пока нечего продолжить. Здесь появится то, что вы начнёте смотреть.",
    remove: (title: string) => `Убрать «${title}» из раздела «Продолжить просмотр»`,
    removeHint: "Убрать из раздела «Продолжить просмотр»",
    episodes: "Серии",
    details: "Подробнее",
    recent: "Недавно добавленные",
    seeLibrary: "Открыть библиотеку",
    libraryEmpty: "Библиотека пуста.",
  },
  library: {
    title: "Библиотека",
    search: "Поиск",
    searchPlaceholder: "Поиск по названию",
    kind: "Тип",
    all: "Все",
    allCount: (count: number) => `Все (${count})`,
    movies: "Фильмы",
    series: "Сериалы",
    other: "Другое",
    category: "Категория",
    uncategorized: (count: number) => `Без категории (${count})`,
    genre: "Жанр",
    nothingMatches: "Ничего не найдено.",
    empty: "Библиотека пуста.",
    showMore: "Показать ещё",
  },
  title: {
    notFound: "Торрент не найден",
    notFoundHint: { before: "Возможно, его удалили. Откройте ", link: "библиотеку", after: "." },
    genres: "Жанры",
    seasons: "Сезоны",
    findingFiles: "Поиск файлов торрента…",
    noVideos: "В торренте нет видеофайлов.",
    videos: "Видео",
    otherVideos: "Другие видео",
  },
  profiles: {
    who: "Выберите профиль",
    remembered: "Устройство запомнит выбор. Сменить профиль можно в шапке.",
    none: "Нет профилей для просмотра.",
    readFailed: "Не удалось загрузить профили.",
    device: "Устройства",
    default: "Основной",
  },
  signIn: {
    title: "Вход",
    password: "Пароль",
    submit: "Войти",
    busy: "Вход…",
    tokenName: "Веб-браузер",
  },
  notFound: {
    title: "Страница не найдена",
    body: (path: string) => `В TorrPlay нет страницы ${path}.`,
    home: "На главную",
  },
  problems: {
    "invalid-credentials": "Неверный пароль.",
    "too-many-attempts": "Слишком много попыток. Подождите немного и попробуйте ещё раз.",
    "engine-unavailable":
      "TorrPlay запускается или останавливается. Попробуйте ещё раз чуть позже.",
    "internal-error": "В TorrPlay произошла ошибка.",
  },
};
