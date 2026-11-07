// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

// What the contract allows and the forms check before asking; limits.test.ts
// compares these with `api/openapi.yaml`.

/** The largest poster or avatar. */
export const IMAGE_MAX_BYTES = 1_048_576;
/** The image types a poster or avatar may be, as a file picker's filter. */
export const IMAGE_TYPES = "image/avif,image/gif,image/jpeg,image/png,image/webp";
/** The longest name of a profile or an access token. */
export const NAME_MAX_LENGTH = 64;
export const PASSWORD_MIN_LENGTH = 8;
export const PASSWORD_MAX_LENGTH = 256;
/** The years a torrent's release may be in. */
export const YEAR_MIN = 1800;
export const YEAR_MAX = 2200;
/** A title's IMDb ID. */
export const IMDB_ID = /^tt[0-9]{7,10}$/;
/** The longest text the log can be searched for. */
export const LOG_SEARCH_MAX_LENGTH = 256;
