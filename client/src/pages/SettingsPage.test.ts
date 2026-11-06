// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

import { fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";

import { ApiError } from "../lib/api/http";
import type { Backend } from "../lib/backend";
import { backendContext } from "../lib/context";
import { location, navigate, startNavigation } from "../lib/navigation.svelte";
import type { SettingSection } from "../lib/settings";
import { fakeBackend, settings } from "../testing";
import SettingsPage from "./SettingsPage.svelte";

function show(backend: Backend, section: SettingSection | undefined = "playback") {
  render(SettingsPage, { props: { section }, context: backendContext(backend) });
}

async function type(label: string, value: string): Promise<void> {
  await fireEvent.input(await screen.findByLabelText(label), { target: { value } });
}

function refusal(status: number, members: object): ApiError {
  return new ApiError(status, {
    type: "tag:skunkie.github.io,2026:torrplay/problems/validation-failed",
    title: "Refused",
    status,
    ...members,
  });
}

it("shows the stored values in their units", async () => {
  show(fakeBackend());

  expect(await screen.findByLabelText("Memory for pieces")).toHaveValue("256");
  expect(screen.getByLabelText("Remove when unwatched for")).toHaveValue("10");
  expect(screen.getByRole("switch", { name: "Download in the background" })).toHaveAttribute(
    "aria-checked",
    "true",
  );
});

it("offers saving only once something changed, and saves only that", async () => {
  const backend = fakeBackend();
  show(backend);
  await screen.findByLabelText("Memory for pieces");
  expect(screen.queryByRole("button", { name: "Save" })).not.toBeInTheDocument();

  await type("Memory for pieces", "512");
  expect(screen.getByText(/1\s*change/)).toBeInTheDocument();
  await fireEvent.click(screen.getByRole("button", { name: "Save" }));

  await waitFor(() => {
    expect(backend.updateSettings).toHaveBeenCalledWith(
      { cache_capacity_bytes: 512 * 1024 * 1024 },
      '"s"',
    );
  });
  expect(await screen.findByText("Saved")).toBeInTheDocument();
});

it("reads the settings again after saving", async () => {
  let stored = settings();
  const backend = fakeBackend({
    getSettings: vi.fn(async () => ({ data: stored, etag: '"s"' })),
    updateSettings: vi.fn(async () => {
      stored = settings({ peer_limit: 80 });
    }),
  });
  show(backend, "network");

  await type("Peers per torrent", "80");
  await fireEvent.click(screen.getByRole("button", { name: "Save" }));

  await screen.findByText("Saved");
  expect(screen.getByLabelText("Peers per torrent")).toHaveValue("80");
  expect(screen.queryByRole("button", { name: "Save" })).not.toBeInTheDocument();
});

it("toggles a switch", async () => {
  const backend = fakeBackend();
  show(backend, "network");

  await fireEvent.click(await screen.findByRole("switch", { name: "Share with TVs over DLNA" }));
  await fireEvent.click(screen.getByRole("button", { name: "Save" }));

  await waitFor(() => {
    expect(backend.updateSettings).toHaveBeenCalledWith({ dlna_enabled: true }, '"s"');
  });
});

it("names a field's unit inside it, for those who cannot see it too", async () => {
  show(fakeBackend());

  const memory = await screen.findByLabelText("Memory for pieces");
  expect(memory).toHaveAccessibleDescription(expect.stringContaining("MiB"));
  expect(screen.getByLabelText("Idle readers in all")).toHaveAttribute(
    "aria-describedby",
    "setting-idle_reader_limit-hint",
  );
});

it("says what turning on DLNA gives everyone on the network", async () => {
  show(fakeBackend(), "network");

  expect(await screen.findByText(/without a password, even when one is set/)).toBeInTheDocument();
});

it("discards what was typed", async () => {
  const backend = fakeBackend();
  show(backend);

  await type("Memory for pieces", "512");
  await fireEvent.click(screen.getByRole("button", { name: "Discard" }));

  expect(screen.getByLabelText("Memory for pieces")).toHaveValue("256");
  expect(backend.updateSettings).not.toHaveBeenCalled();
  expect(screen.queryByRole("button", { name: "Save" })).not.toBeInTheDocument();
});

it("holds back saving a value out of bounds, saying why", async () => {
  const backend = fakeBackend();
  show(backend);

  await type("Wait for a missing piece", "9000");

  expect(screen.getByRole("alert")).toHaveTextContent("Enter a whole number from 1 to 600.");
  expect(screen.getByRole("button", { name: "Save" })).toBeDisabled();
  expect(screen.getByLabelText("Wait for a missing piece")).toHaveAttribute("aria-invalid", "true");
});

it("shows what the server refuses beside its setting and keeps what was typed", async () => {
  const backend = fakeBackend({
    updateSettings: vi.fn(async () => {
      throw refusal(422, {
        errors: [
          { pointer: "/cache_capacity_bytes", detail: "Must hold the largest torrent's piece." },
        ],
      });
    }),
  });
  show(backend);

  await type("Memory for pieces", "4");
  await fireEvent.click(screen.getByRole("button", { name: "Save" }));

  expect(await screen.findByRole("alert")).toHaveTextContent(
    "Must hold the largest torrent's piece.",
  );
  expect(screen.getByLabelText("Memory for pieces")).toHaveValue("4");
});

it("keeps what was typed when the settings changed elsewhere, over the new ones", async () => {
  let stored = settings();
  let tag = '"s"';
  const updateSettings = vi.fn(async (_patch: object, etag: string) => {
    if (etag !== '"s2"') {
      stored = settings({ idle_reader_limit: 20 });
      tag = '"s2"';
      throw refusal(412, {});
    }
  });
  const backend = fakeBackend({
    getSettings: vi.fn(async () => ({ data: stored, etag: tag })),
    updateSettings,
  });
  show(backend);

  await type("Memory for pieces", "512");
  await fireEvent.click(screen.getByRole("button", { name: "Save" }));

  expect(await screen.findByText(/changed somewhere else/)).toBeInTheDocument();
  expect(screen.getByLabelText("Memory for pieces")).toHaveValue("512");
  expect(screen.getByLabelText("Idle readers in all")).toHaveValue("20");

  await fireEvent.click(screen.getByRole("button", { name: "Save" }));

  await waitFor(() => {
    expect(updateSettings).toHaveBeenLastCalledWith(
      { cache_capacity_bytes: 512 * 1024 * 1024 },
      '"s2"',
    );
  });
});

it("reports a failed save and keeps what was typed", async () => {
  const backend = fakeBackend({
    updateSettings: vi.fn(async () => {
      throw new TypeError("offline");
    }),
  });
  show(backend);

  await type("Memory for pieces", "512");
  await fireEvent.click(screen.getByRole("button", { name: "Save" }));

  expect(await screen.findByText("TorrPlay could not be reached.")).toBeInTheDocument();
  expect(screen.getByLabelText("Memory for pieces")).toHaveValue("512");
  expect(screen.getByRole("button", { name: "Save" })).toBeEnabled();
});

it("offers to read again when the settings could not be read", async () => {
  const getSettings = vi
    .fn<Backend["getSettings"]>()
    .mockRejectedValueOnce(new TypeError("offline"))
    .mockResolvedValue({ data: settings(), etag: '"s"' });
  show(fakeBackend({ getSettings }));

  expect(await screen.findByRole("alert")).toHaveTextContent("TorrPlay could not be reached.");
  await fireEvent.click(screen.getByRole("button", { name: "Try again" }));

  expect(await screen.findByLabelText("Memory for pieces")).toHaveValue("256");
});

it("disables DHT while a command-line option keeps it off, saying so", async () => {
  show(
    fakeBackend({
      getSettings: vi.fn(async () => ({
        data: settings({ overrides: { dht_enabled: false } }),
        etag: '"s"',
      })),
    }),
    "network",
  );

  const dht = await screen.findByRole("switch", { name: "Find peers through DHT" });
  expect(dht).toBeDisabled();
  expect(dht).toHaveAttribute("aria-checked", "false");
  expect(screen.getByText(/--no-dht/)).toBeInTheDocument();
});

it("keeps what was typed when moving to another section", async () => {
  const { rerender } = render(SettingsPage, {
    props: { section: "playback" as SettingSection },
    context: backendContext(fakeBackend()),
  });

  await type("Memory for pieces", "512");
  await rerender({ section: "network" });
  expect(await screen.findByLabelText("Peers per torrent")).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Save" })).toBeInTheDocument();

  await rerender({ section: "playback" });
  expect(screen.getByLabelText("Memory for pieces")).toHaveValue("512");
});

it("lists the sections on their own page, with none of them current", async () => {
  render(SettingsPage, { props: { section: undefined }, context: backendContext(fakeBackend()) });

  const nav = await screen.findByRole("navigation", { name: "Settings" });
  expect(nav.querySelector('a[href="/settings/playback"]')).toHaveTextContent("Playback and cache");
  expect(nav.querySelector('a[href="/settings/network"]')).toHaveTextContent("Network and DLNA");
  expect(nav.querySelector("[aria-current]")).toBeNull();
});

it("saves when the form is submitted, as Enter does, unless a value is wrong", async () => {
  const backend = fakeBackend();
  show(backend);

  await type("Wait for a missing piece", "9000");
  const form = screen.getByLabelText("Memory for pieces").closest("form");
  if (!form) throw new Error("the settings are a form");
  await fireEvent.submit(form);
  expect(backend.updateSettings).not.toHaveBeenCalled();

  await type("Wait for a missing piece", "90");
  await fireEvent.submit(form);
  await waitFor(() => {
    expect(backend.updateSettings).toHaveBeenCalledWith({ piece_wait_seconds: 90 }, '"s"');
  });
});

it("lets the other settings be saved past a value it can only round", async () => {
  const backend = fakeBackend({
    getSettings: vi.fn(async () => ({
      data: settings({ cache_capacity_bytes: 300_000_000 }),
      etag: '"s"',
    })),
  });
  show(backend);

  await type("Wait for a missing piece", "90");
  await fireEvent.click(screen.getByRole("button", { name: "Save" }));

  await waitFor(() => {
    expect(backend.updateSettings).toHaveBeenCalledWith({ piece_wait_seconds: 90 }, '"s"');
  });
});

it("marks a section with a problem while another is shown", async () => {
  const { rerender } = render(SettingsPage, {
    props: { section: "playback" as SettingSection },
    context: backendContext(fakeBackend()),
  });

  await type("Wait for a missing piece", "9000");
  await rerender({ section: "network" });

  expect(
    screen.getByRole("link", { name: /Playback and cache.*needs fixing/ }),
  ).toBeInTheDocument();
  expect(screen.getByRole("link", { name: "Network and DLNA" })).toBeInTheDocument();
  expect(screen.getByText(/Some values need fixing/)).toBeInTheDocument();
});

it("says so when the server refuses a value on a section not shown", async () => {
  const backend = fakeBackend({
    updateSettings: vi.fn(async () => {
      throw refusal(422, { errors: [{ pointer: "/cache_capacity_bytes", detail: "Too small." }] });
    }),
  });
  const { rerender } = render(SettingsPage, {
    props: { section: "playback" as SettingSection },
    context: backendContext(backend),
  });

  await type("Memory for pieces", "4");
  await rerender({ section: "network" });
  await fireEvent.click(screen.getByRole("button", { name: "Save" }));

  expect(await screen.findByText(/Some values need fixing/)).toBeInTheDocument();
  expect(
    screen.getByRole("link", { name: /Playback and cache.*needs fixing/ }),
  ).toBeInTheDocument();
});

it("forgets a refusal once the value changes", async () => {
  const backend = fakeBackend({
    updateSettings: vi.fn(async () => {
      throw refusal(422, { errors: [{ pointer: "/cache_capacity_bytes", detail: "Too small." }] });
    }),
  });
  show(backend);
  await type("Memory for pieces", "4");
  await fireEvent.click(screen.getByRole("button", { name: "Save" }));
  await screen.findByRole("alert");

  await type("Memory for pieces", "8");

  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
});

it("keeps what is typed while a save is running", async () => {
  let finish = () => {};
  let stored = settings();
  const backend = fakeBackend({
    getSettings: vi.fn(async () => ({ data: stored, etag: '"s"' })),
    updateSettings: vi.fn(
      () =>
        new Promise<void>((resolve) => {
          finish = () => {
            stored = settings({ cache_capacity_bytes: 512 * 1024 * 1024 });
            resolve();
          };
        }),
    ),
  });
  show(backend);

  await type("Memory for pieces", "512");
  await fireEvent.click(screen.getByRole("button", { name: "Save" }));
  await type("Wait for a missing piece", "90");
  finish();

  await waitFor(() => {
    expect(screen.getByLabelText("Memory for pieces")).toHaveValue("512");
    expect(screen.getByText(/1\s*change/)).toBeInTheDocument();
  });
  expect(screen.getByLabelText("Wait for a missing piece")).toHaveValue("90");
});

it("ties an error to its field, for those who cannot see it", async () => {
  show(fakeBackend());

  await type("Wait for a missing piece", "9000");

  expect(screen.getByLabelText("Wait for a missing piece")).toHaveAccessibleDescription(
    expect.stringContaining("Enter a whole number from 1 to 600."),
  );
});

describe("with unsaved changes", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
    navigate("/");
  });

  function link(path: string): HTMLAnchorElement {
    const anchor = document.createElement("a");
    anchor.href = path;
    document.body.append(anchor);
    return anchor;
  }

  async function changed(): Promise<() => void> {
    const stop = startNavigation();
    navigate("/settings/playback");
    show(fakeBackend());
    await type("Memory for pieces", "512");
    return stop;
  }

  it("asks before a link leaves the settings, and stays when told to", async () => {
    const stop = await changed();
    const confirm = vi.fn(() => false);
    vi.stubGlobal("confirm", confirm);

    await fireEvent.click(link("/library"));

    expect(confirm).toHaveBeenCalledWith("Leave without saving your changes?");
    expect(location.path).toBe("/settings/playback");
    stop();
  });

  it("leaves when told to", async () => {
    const stop = await changed();
    vi.stubGlobal(
      "confirm",
      vi.fn(() => true),
    );

    await fireEvent.click(link("/library"));

    expect(location.path).toBe("/library");
    stop();
  });

  it("does not ask when moving between the sections", async () => {
    const stop = await changed();
    const confirm = vi.fn(() => false);
    vi.stubGlobal("confirm", confirm);

    await fireEvent.click(link("/settings/network"));

    expect(confirm).not.toHaveBeenCalled();
    expect(location.path).toBe("/settings/network");
    stop();
  });

  it("asks before moving to a page of the settings that is not this form", async () => {
    const stop = await changed();
    const confirm = vi.fn(() => false);
    vi.stubGlobal("confirm", confirm);

    await fireEvent.click(link("/settings/profiles"));

    expect(confirm).toHaveBeenCalled();
    expect(location.path).toBe("/settings/playback");
    stop();
  });

  it("warns before the page unloads, until nothing is unsaved", async () => {
    const stop = await changed();
    const unload = () => {
      const event = new Event("beforeunload", { cancelable: true });
      window.dispatchEvent(event);
      return event.defaultPrevented;
    };
    expect(unload()).toBe(true);

    await fireEvent.click(screen.getByRole("button", { name: "Discard" }));

    expect(unload()).toBe(false);
    stop();
  });
});
