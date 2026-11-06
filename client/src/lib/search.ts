// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import type { Torrent } from "./backend";

// A port of the server's matcher without typo tolerance, which only the server
// has: `crates/torrplay-core/src/search.rs` and the lifecycle's Search section
// say what it does, and `fixtures/search-cases.json` holds the cases both pass.

const WHITESPACE = /\p{White_Space}+/u;
const WORD_CHARACTER = /^[\p{L}\p{Nd}]$/u;
const MARK = /^\p{Mn}$/u;

/** How much each way of matching a word adds to a match's rank. */
const WHOLE_WORD = 4;
const WORD_START = 3;
const INSIDE_WORDS = 2;

/** A search, split into its words once to match many items. */
interface SearchQuery {
  words: string[];
  /** The folded parts of a search without words, such as `-`, which match as
   * plain text instead. */
  textParts: string[];
}

/** `character` in the case Unicode's full case folding gives it. JavaScript has
 * no folding, but lowercasing the uppercase form reaches the same result, apart
 * from a dotless i, whose uppercase is a plain I. Cherokee also folds differently,
 * to lowercase rather than uppercase, which changes no result: words lowercase
 * each letter anyway, and a search without words holds no letters to compare.
 * A character goes alone so that a capital sigma never becomes a final one. */
function foldCharacter(character: string): string {
  if (character === "ı") return character;
  return character.toLowerCase().toUpperCase().toLowerCase();
}

function fold(text: string): string {
  let folded = "";
  for (const character of text) folded += foldCharacter(character);
  return folded;
}

/** `text` folded and in plain forms, for a search without words. */
function foldText(text: string): string {
  return fold(text).normalize("NFKC").replaceAll("’", "'");
}

function splitWhitespace(text: string): string[] {
  return text.split(WHITESPACE).filter((part) => part !== "");
}

/** The folded words of `text`, free of diacritics and compatibility forms.
 * Folding again after the decomposition folds compatibility forms that
 * decompose to capitals; each letter is then lowercased, since folding keeps
 * some letters in uppercase. */
function splitWords(text: string): string[] {
  let spaced = "";
  for (const character of fold(fold(text).normalize("NFKD"))) {
    if (MARK.test(character) || character === "'" || character === "’") continue;
    spaced += WORD_CHARACTER.test(character) ? character.toLowerCase() : " ";
  }
  return splitWhitespace(spaced);
}

function parseSearch(text: string): SearchQuery {
  const words = splitWords(text);
  return { words, textParts: words.length > 0 ? [] : splitWhitespace(foldText(text)) };
}

function wordQuality(word: string, itemWords: string[], joined: string): number | undefined {
  if (itemWords.includes(word)) return WHOLE_WORD;
  if (itemWords.some((itemWord) => itemWord.startsWith(word))) return WORD_START;
  return joined.includes(word) ? INSIDE_WORDS : undefined;
}

/** How well `fields` match every word of `query`, higher being better, or
 * `undefined` if one does not match. A blank search matches anything, and a
 * search of punctuation alone matches fields containing each of its parts. */
function rankMatch(query: SearchQuery, fields: string[]): number | undefined {
  if (query.words.length === 0) {
    const folded = fields.map(foldText);
    const all = query.textParts.every((part) => folded.some((field) => field.includes(part)));
    return all ? 0 : undefined;
  }
  const itemWords = splitWords(fields.join(" "));
  const joined = itemWords.join(" ");
  let rank = 0;
  for (const word of query.words) {
    const quality = wordQuality(word, itemWords, joined);
    if (quality === undefined) return undefined;
    rank += quality;
  }
  return rank;
}

/** The `items` whose `fields` match `text`, best first; equal matches keep their
 * order. */
export function searchItems<T>(items: T[], text: string, fields: (item: T) => string[]): T[] {
  const query = parseSearch(text);
  return items
    .flatMap((item) => {
      const rank = rankMatch(query, fields(item));
      return rank === undefined ? [] : [{ item, rank }];
    })
    .sort((a, b) => b.rank - a.rank)
    .map(({ item }) => item);
}

/** The `torrents` matching `text`, best first, by title and name. */
export function searchTorrents(torrents: Torrent[], text: string): Torrent[] {
  return searchItems(torrents, text, ({ title, name }) =>
    [title, name].filter((field) => field !== null && field !== undefined),
  );
}
