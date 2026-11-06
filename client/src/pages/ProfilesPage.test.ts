// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { fireEvent, render, screen, waitFor, within } from "@testing-library/svelte";
import { beforeEach, expect, it, vi } from "vitest";

import { ApiError } from "../lib/api/http";
import type { Backend, Profile } from "../lib/backend";
import { backendContext } from "../lib/context";
import {
  avatarUrl,
  chooseProfile,
  loadProfiles,
  profiles,
  refreshAvatarGrant,
  resetProfiles,
} from "../lib/profile.svelte";
import { fakeBackend, profile } from "../testing";
import ProfilesPage from "./ProfilesPage.svelte";

const anna = profile(1, "Anna");
const ben = profile(2, "Ben");
const tv = profile(3, "Devices", "device");

function backendOf(members: Partial<Backend> = {}, list: Profile[] = [anna, ben, tv]): Backend {
  return fakeBackend({ listProfiles: vi.fn(async () => [...list]), ...members });
}

function show(backend: Backend) {
  render(ProfilesPage, { context: backendContext(backend) });
}

function refused(status: number, name: string, detail: string): ApiError {
  return new ApiError(status, {
    type: `tag:skunkie.github.io,2026:torrplay/problems/${name}`,
    title: detail,
    detail,
    status,
  });
}

beforeEach(() => {
  resetProfiles();
  localStorage.clear();
});

it("lists everyone, marking who watches on this device", async () => {
  chooseProfile(anna);
  show(backendOf());

  expect(await screen.findByText("Anna")).toBeInTheDocument();
  expect(screen.getByText("Ben")).toBeInTheDocument();
  expect(screen.getByText("Watching on this device")).toBeInTheDocument();
});

it("offers the device profile nothing to change, as the contract says", async () => {
  show(backendOf());

  const row = (await screen.findByText(/Holds the progress/)).closest("li");
  if (!row) throw new Error("the device has a row");
  expect(within(row).queryByRole("button")).not.toBeInTheDocument();
});

it("does not offer deleting the last person", async () => {
  show(backendOf({}, [anna, tv]));

  await screen.findByText("Anna");
  expect(screen.queryByRole("button", { name: "Delete Anna" })).not.toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Rename Anna" })).toBeInTheDocument();
});

it("adds a profile and clears the field", async () => {
  const backend = backendOf({ createProfile: vi.fn(async () => profile(4, "Cleo")) });
  show(backend);
  const field = await screen.findByLabelText("New profile");

  await fireEvent.input(field, { target: { value: "  Cleo " } });
  await fireEvent.click(screen.getByRole("button", { name: "Add" }));

  await waitFor(() => {
    expect(backend.createProfile).toHaveBeenCalledWith("Cleo");
    expect(field).toHaveValue("");
  });
});

it("says why a name is refused and keeps it", async () => {
  const backend = backendOf({
    createProfile: vi.fn(async () => {
      throw refused(409, "duplicate-profile", "From the server.");
    }),
  });
  show(backend);
  const field = await screen.findByLabelText("New profile");

  await fireEvent.input(field, { target: { value: "Anna" } });
  await fireEvent.click(screen.getByRole("button", { name: "Add" }));

  expect(await screen.findByRole("alert")).toHaveTextContent("Another profile has this name.");
  expect(field).toHaveValue("Anna");
});

it("renames from a field that starts with the name, and shows others the change", async () => {
  const list = [anna, ben, tv];
  const backend = backendOf(
    {
      renameProfile: vi.fn(async () => {
        list[0] = { ...anna, name: "Anya" };
      }),
    },
    list,
  );
  chooseProfile(anna);
  show(backend);

  await fireEvent.click(await screen.findByRole("button", { name: "Rename Anna" }));
  const field = screen.getByLabelText("New name for Anna");
  expect(field).toHaveValue("Anna");
  await fireEvent.input(field, { target: { value: "Anya" } });
  await fireEvent.click(screen.getByRole("button", { name: "Save" }));

  expect(await screen.findByText("Anya")).toBeInTheDocument();
  expect(backend.renameProfile).toHaveBeenCalledWith(anna.id, "Anya");
  await waitFor(() => {
    expect(profiles.current?.name).toBe("Anya");
  });
  expect(screen.queryByLabelText("New name for Anna")).not.toBeInTheDocument();
});

it("keeps the field open when a rename is refused", async () => {
  const backend = backendOf({
    renameProfile: vi.fn(async () => {
      throw refused(409, "duplicate-profile", "From the server.");
    }),
  });
  show(backend);

  await fireEvent.click(await screen.findByRole("button", { name: "Rename Anna" }));
  await fireEvent.input(screen.getByLabelText("New name for Anna"), { target: { value: "Ben" } });
  await fireEvent.click(screen.getByRole("button", { name: "Save" }));

  expect(await screen.findByRole("alert")).toHaveTextContent("Another profile has this name.");
  expect(screen.getByLabelText("New name for Anna")).toHaveValue("Ben");
});

it.each(["Save", "Cancel"])("goes back to the Rename button after %s", async (action) => {
  show(backendOf());

  await fireEvent.click(await screen.findByRole("button", { name: "Rename Anna" }));
  await fireEvent.click(screen.getByRole("button", { name: action }));

  await waitFor(() => {
    expect(screen.getByRole("button", { name: "Rename Anna" })).toHaveFocus();
  });
});

it("says a blank name needs a name, for a new profile and a rename", async () => {
  const backend = backendOf();
  show(backend);

  await fireEvent.input(await screen.findByLabelText("New profile"), { target: { value: "  " } });
  await fireEvent.click(screen.getByRole("button", { name: "Add" }));
  expect(screen.getByRole("alert")).toHaveTextContent("Enter a name.");
  expect(backend.createProfile).not.toHaveBeenCalled();

  await fireEvent.input(screen.getByLabelText("New profile"), { target: { value: "" } });
  await fireEvent.click(screen.getByRole("button", { name: "Rename Ben" }));
  await fireEvent.input(screen.getByLabelText("New name for Ben"), { target: { value: " " } });
  await fireEvent.click(screen.getByRole("button", { name: "Save" }));

  expect(screen.getAllByRole("alert").at(0)).toHaveTextContent("Enter a name.");
  expect(backend.renameProfile).not.toHaveBeenCalled();
  expect(screen.getByLabelText("New name for Ben")).toBeInTheDocument();
});

it("asks for a new grant, and keeps the one held if that fails", async () => {
  const grant = { token: "tpk_a", expiresAt: new Date(Date.now() + 86_400_000) };
  const playbackGrant = vi.fn(async () => grant);
  const backend = fakeBackend({ playbackGrant });
  await refreshAvatarGrant(backend);
  expect(profiles.grant).toEqual(grant);

  const failing = fakeBackend({
    playbackGrant: vi.fn(async () => {
      throw new TypeError("offline");
    }),
  });
  await refreshAvatarGrant(failing);

  expect(profiles.grant).toEqual(grant);
});

it("cancels a rename", async () => {
  const backend = backendOf();
  show(backend);

  await fireEvent.click(await screen.findByRole("button", { name: "Rename Anna" }));
  await fireEvent.click(screen.getByRole("button", { name: "Cancel" }));

  expect(screen.getByText("Anna")).toBeInTheDocument();
  expect(backend.renameProfile).not.toHaveBeenCalled();
});

it("asks before deleting, saying what goes with it", async () => {
  const backend = backendOf();
  show(backend);

  await fireEvent.click(await screen.findByRole("button", { name: "Delete Ben" }));
  const dialog = screen.getByRole("dialog", { name: "Delete Ben?" });
  expect(dialog).toHaveTextContent("Their progress and up next are deleted with them.");
  expect(backend.deleteProfile).not.toHaveBeenCalled();

  await fireEvent.click(within(dialog).getByRole("button", { name: "Delete" }));

  await waitFor(() => {
    expect(backend.deleteProfile).toHaveBeenCalledWith(ben.id);
  });
});

it("stops watching as a profile that was deleted", async () => {
  const list = [anna, ben, tv];
  const backend = backendOf(
    {
      deleteProfile: vi.fn(async () => {
        list.splice(1, 1);
      }),
    },
    list,
  );
  chooseProfile(ben);
  show(backend);

  await fireEvent.click(await screen.findByRole("button", { name: "Delete Ben" }));
  await fireEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Delete" }));

  await waitFor(() => {
    expect(profiles.current?.id).toBe(anna.id);
  });
});

it("uploads a chosen avatar", async () => {
  const backend = backendOf();
  show(backend);
  const image = new File(["x"], "a.png", { type: "image/png" });

  await fireEvent.change(await screen.findByLabelText("Choose an avatar for Anna"), {
    target: { files: [image] },
  });

  await waitFor(() => {
    expect(backend.setAvatar).toHaveBeenCalledWith(anna.id, image);
  });
});

it("refuses an avatar over the limit without sending it, saying why", async () => {
  const backend = backendOf();
  show(backend);
  const image = new File([new Uint8Array(1048577)], "a.png", { type: "image/png" });

  await fireEvent.change(await screen.findByLabelText("Choose an avatar for Ben"), {
    target: { files: [image] },
  });

  expect(await screen.findByRole("alert")).toHaveTextContent("at most 1");
  expect(backend.setAvatar).not.toHaveBeenCalled();
  const row = screen.getByText("Ben").closest("li");
  expect(row).toContainElement(screen.getByRole("alert"));
});

it("says why the server refuses an avatar", async () => {
  const backend = backendOf({
    setAvatar: vi.fn(async () => {
      throw refused(415, "unsupported-media-type", "From the server.");
    }),
  });
  show(backend);

  await fireEvent.change(await screen.findByLabelText("Choose an avatar for Anna"), {
    target: { files: [new File(["x"], "a.png", { type: "image/png" })] },
  });

  expect(await screen.findByRole("alert")).toHaveTextContent("not an image TorrPlay can use");
});

it("offers removing an avatar only when there is one", async () => {
  const withAvatar: Profile = {
    ...anna,
    avatar: { media_type: "image/png", size_bytes: 1, updated_at: "2026-11-06T00:00:00Z" },
  };
  const backend = backendOf({}, [withAvatar, ben, tv]);
  show(backend);

  await fireEvent.click(await screen.findByRole("button", { name: "Remove the avatar of Anna" }));

  await waitFor(() => {
    expect(backend.deleteAvatar).toHaveBeenCalledWith(anna.id);
  });
  expect(
    screen.queryByRole("button", { name: "Remove the avatar of Ben" }),
  ).not.toBeInTheDocument();
});

it("offers to read again when the profiles could not be read", async () => {
  const listProfiles = vi
    .fn<Backend["listProfiles"]>()
    .mockRejectedValueOnce(new TypeError("offline"))
    .mockResolvedValue([anna, tv]);
  show(fakeBackend({ listProfiles }));

  expect(await screen.findByRole("alert")).toHaveTextContent("could not be reached");
  await fireEvent.click(screen.getByRole("button", { name: "Try again" }));

  expect(await screen.findByText("Anna")).toBeInTheDocument();
});

const withAvatar: Profile = {
  ...anna,
  avatar: { media_type: "image/png", size_bytes: 1, updated_at: "2026-11-06T00:00:00Z" },
};

it("carries a grant's token when one is needed", () => {
  const versioned = { ...withAvatar, links: { ...withAvatar.links, avatar: "/avatar?v=1" } };

  expect(avatarUrl(versioned, undefined)).toBe("/avatar?v=1");
  expect(avatarUrl(versioned, { token: "tpk_a", expiresAt: new Date() })).toBe(
    "/avatar?v=1&token=tpk_a",
  );
});

it("has no URL without an avatar or before the grant is known", () => {
  expect(avatarUrl(anna, undefined)).toBeUndefined();
  expect(avatarUrl(withAvatar, null)).toBeUndefined();
});

it("shows the avatar once the grant is known, and the initial if it cannot load", async () => {
  await loadProfiles(backendOf({}, [withAvatar, tv]));
  show(backendOf({}, [withAvatar, tv]));
  await screen.findByText("Anna");
  const row = screen.getByText("Anna").closest("li");
  const image = row?.querySelector("img");
  if (!image) throw new Error("the avatar is shown");
  expect(image).toHaveAttribute("src", anna.links.avatar);

  await fireEvent.error(image);

  expect(row?.querySelector("img")).toBeNull();
  expect(row).toHaveTextContent("A");
});
