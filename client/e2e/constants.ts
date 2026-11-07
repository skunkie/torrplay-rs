// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

/** Where the rig runs the application for the browser tests. */
// Below Linux's range of ports handed out to whatever asks for any port, where one of
// these could be taken after the rig has found it free.
export const APPLICATION_URL = "http://127.0.0.1:17910";

/** The file the local swarm seeds. */
export const TORRENT_NAME = "test-pattern.webm";
