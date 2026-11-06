// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { fireEvent, render, screen, waitFor, within } from "@testing-library/svelte";
import { afterEach, beforeEach, expect, it, vi } from "vitest";

import { ApiError } from "../lib/api/http";
import type { Backend } from "../lib/backend";
import { backendContext } from "../lib/context";
import { loadProfiles, profiles, resetProfiles } from "../lib/profile.svelte";
import { fakeBackend, profile } from "../testing";
import BackupPage from "./BackupPage.svelte";

const ARCHIVE = new File(["PK"], "library.zip", { type: "application/zip" });

function show(backend: Backend) {
  render(BackupPage, { context: backendContext(backend) });
}

function refused(status: number, name: string, detail: string, members: object = {}): ApiError {
  return new ApiError(status, {
    type: `tag:skunkie.github.io,2026:torrplay/problems/${name}`,
    title: detail,
    detail,
    status,
    ...members,
  });
}

async function choose(): Promise<void> {
  await fireEvent.change(screen.getByLabelText("Choose a file"), {
    target: { files: [ARCHIVE] },
  });
}

beforeEach(() => {
  resetProfiles();
  URL.createObjectURL = vi.fn(() => "blob:backup");
  URL.revokeObjectURL = vi.fn();
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.useRealTimers();
});

it("downloads the backup under the name the server gave it", async () => {
  const archive = new Blob(["PK"]);
  const names: string[] = [];
  vi.useFakeTimers({ toFake: ["setTimeout"], shouldAdvanceTime: true });
  vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(function (
    this: HTMLAnchorElement,
  ) {
    names.push(this.download);
  });
  show(fakeBackend({ getBackup: vi.fn(async () => ({ archive, name: "from-server.zip" })) }));

  await fireEvent.click(screen.getByRole("button", { name: "Download" }));

  await waitFor(() => expect(names).toEqual(["from-server.zip"]));
  expect(URL.createObjectURL).toHaveBeenCalledWith(archive);
  // Some browsers read the URL after the click, so it outlasts it.
  expect(URL.revokeObjectURL).not.toHaveBeenCalled();
  vi.advanceTimersByTime(60_000);
  expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:backup");
});

it("says why a download failed", async () => {
  show(
    fakeBackend({
      getBackup: vi.fn(async () => {
        throw refused(507, "insufficient-storage", "The backup needs more.");
      }),
    }),
  );
  await fireEvent.click(screen.getByRole("button", { name: "Download" }));

  expect((await screen.findByRole("alert")).textContent).toBe(
    "The data folder has too little free space for the backup.",
  );
  expect(screen.getByRole("button", { name: "Download" })).toHaveProperty("disabled", false);
});

it("restores only after asking, and reads the profiles again", async () => {
  await loadProfiles(fakeBackend({ listProfiles: vi.fn(async () => [profile(1, "Anna")]) }));
  const backend = fakeBackend();
  show(backend);

  await choose();
  const dialog = screen.getByRole("dialog");
  expect(dialog.textContent).toContain("library.zip");
  expect(dialog.textContent).toContain("deleted, with their downloaded files");
  expect(backend.restoreBackup).not.toHaveBeenCalled();
  await fireEvent.click(within(dialog).getByRole("button", { name: "Restore" }));

  expect((await screen.findByRole("status")).textContent).toContain("Restored");
  expect(backend.restoreBackup).toHaveBeenCalledWith(ARCHIVE);
  expect(profiles.people).toBeUndefined();
});

it("changes nothing when the question is declined", async () => {
  const backend = fakeBackend();
  show(backend);
  await choose();
  await fireEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Cancel" }));

  expect(backend.restoreBackup).not.toHaveBeenCalled();
  expect(screen.queryByRole("status")).toBeNull();
});

it.each([
  ["without a member at fault", {}, "This file is not a backup TorrPlay can restore."],
  [
    "naming the member at fault",
    { errors: [{ pointer: "/torrents/0", detail: "The torrent file is missing." }] },
    "This file is not a backup TorrPlay can restore. The torrent file is missing.",
  ],
])("says a file is not a backup, %s", async (_case, members, message) => {
  show(
    fakeBackend({
      restoreBackup: vi.fn(async () => {
        throw refused(422, "validation-failed", "The body is not a zip archive.", members);
      }),
    }),
  );
  await choose();
  await fireEvent.click(
    within(screen.getByRole("dialog")).getByRole("button", { name: "Restore" }),
  );

  expect((await screen.findByRole("alert")).textContent).toBe(message);
  expect(screen.queryByRole("status")).toBeNull();
});
