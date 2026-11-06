// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

// Starts what the browser tests talk to: the real application, built with its
// client, and a local swarm of a tracker and one seeder. Nothing reaches a
// public swarm. Each process's log stays in test-logs/ until the next run.

import { type ChildProcess, spawn } from "node:child_process";
import { createHash } from "node:crypto";
import {
  copyFileSync,
  mkdirSync,
  mkdtempSync,
  openSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { connect, createServer } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { APPLICATION_URL, TORRENT_NAME } from "./constants";

const HOST = "127.0.0.1";
const APPLICATION_PORT = Number(new URL(APPLICATION_URL).port);
const TRACKER_PORT = 47911;
const SEEDER_PORT = 47912;
const PEER_PORT = 47913;
const PIECE_LENGTH = 16_384;
const READY_MS = 30_000;
/** Longer than the application's own limit on shutting down. */
const STOP_MS = 40_000;

const BIN = process.env.TORRPLAY_BIN_DIR ?? join(import.meta.dirname, "../../target/debug");
const LOGS = join(import.meta.dirname, "../test-logs");

type Bencoded = Buffer | number | string | { [key: string]: Bencoded };

/** `value` in BitTorrent's encoding; a dictionary's keys sort by their bytes. */
function bencode(value: Bencoded): Buffer {
  if (typeof value === "number") return Buffer.from(`i${value}e`);
  if (typeof value === "string") return bencode(Buffer.from(value));
  if (Buffer.isBuffer(value)) return Buffer.concat([Buffer.from(`${value.length}:`), value]);
  const members = Object.keys(value)
    .sort()
    .flatMap((key) => [bencode(key), bencode(value[key] as Bencoded)]);
  return Buffer.concat([Buffer.from("d"), ...members, Buffer.from("e")]);
}

function sha1(data: Buffer): Buffer {
  return createHash("sha1").update(data).digest();
}

/** The torrent of `content`, and the magnet link of the same torrent. */
function torrentOf(content: Buffer): { metainfo: Buffer; magnet: string } {
  const pieces: Buffer[] = [];
  for (let start = 0; start < content.length; start += PIECE_LENGTH) {
    pieces.push(sha1(content.subarray(start, start + PIECE_LENGTH)));
  }
  const announce = `http://${HOST}:${TRACKER_PORT}/announce`;
  const info = {
    length: content.length,
    name: TORRENT_NAME,
    "piece length": PIECE_LENGTH,
    pieces: Buffer.concat(pieces),
  };
  return {
    metainfo: bencode({ announce, info }),
    magnet: `magnet:?xt=urn:btih:${sha1(bencode(info)).toString("hex")}&dn=${TORRENT_NAME}&tr=${encodeURIComponent(announce)}`,
  };
}

/** Refuses a port something else holds, such as a server a crashed run left,
 * which the tests would otherwise talk to in place of this run's. */
function assertFree(port: number): Promise<void> {
  return new Promise((resolve, reject) => {
    const probe = createServer();
    probe.once("error", () => {
      reject(new Error(`port ${port} is in use: stop what holds it, perhaps an earlier run`));
    });
    probe.listen(port, HOST, () => probe.close(() => resolve()));
  });
}

function accepts(port: number): Promise<boolean> {
  return new Promise((resolve) => {
    const socket = connect(port, HOST);
    socket.once("connect", () => {
      socket.destroy();
      resolve(true);
    });
    socket.once("error", () => resolve(false));
  });
}

async function applicationReady(): Promise<boolean> {
  try {
    return (await fetch(`${APPLICATION_URL}/readyz`)).ok;
  } catch {
    return false;
  }
}

export default async function setup(): Promise<() => Promise<void>> {
  await Promise.all(
    [APPLICATION_PORT, TRACKER_PORT, SEEDER_PORT, PEER_PORT].map((port) => assertFree(port)),
  );

  rmSync(LOGS, { recursive: true, force: true });
  mkdirSync(LOGS, { recursive: true });
  const work = mkdtempSync(join(tmpdir(), "torrplay-browser-"));
  const seeding = join(work, "seeding");
  mkdirSync(seeding);
  const video = join(import.meta.dirname, "fixtures", "test-pattern.webm");
  copyFileSync(video, join(seeding, TORRENT_NAME));

  const { metainfo, magnet } = torrentOf(readFileSync(video));
  const torrentFile = join(work, "test-pattern.torrent");
  writeFileSync(torrentFile, metainfo);
  // The test workers inherit these.
  process.env.TORRPLAY_E2E_MAGNET = magnet;
  process.env.TORRPLAY_E2E_TORRENT = torrentFile;

  const running: ChildProcess[] = [];
  let stopping = false;
  /** Why a process this run needs is gone. */
  let lost: string | undefined;
  const start = (name: string, command: string, args: string[]) => {
    const logFile = join(LOGS, `${name}.log`);
    const log = openSync(logFile, "w");
    const child = spawn(command, args, { stdio: ["ignore", log, log] });
    const report = (reason: string) => {
      if (stopping) return;
      lost = `${name} ${reason}; see ${logFile}`;
      console.error(lost);
    };
    child.once("error", (error) => report(`did not start: ${error.message}`));
    child.once("exit", (code, signal) => report(`exited early with ${code ?? signal}`));
    running.push(child);
  };

  const stop = async () => {
    stopping = true;
    const killer = setTimeout(() => {
      for (const child of running) child.kill("SIGKILL");
    }, STOP_MS);
    for (const child of running) child.kill();
    await Promise.all(
      running.map((child) =>
        child.exitCode === null && child.signalCode === null
          ? new Promise((resolve) => child.once("exit", resolve))
          : undefined,
      ),
    );
    clearTimeout(killer);
    rmSync(work, { recursive: true, force: true });
  };

  start("tracker", join(BIN, "torrplay-bench"), [
    "tracker",
    "--listen",
    `${HOST}:${TRACKER_PORT}`,
    "--peer",
    `${HOST}:${SEEDER_PORT}`,
  ]);
  start("seeder", "aria2c", [
    `--dir=${seeding}`,
    `--torrent-file=${torrentFile}`,
    `--listen-port=${SEEDER_PORT}`,
    "--check-integrity=true",
    "--seed-ratio=0.0",
    "--enable-dht=false",
    "--enable-dht6=false",
    "--enable-peer-exchange=false",
    "--bt-enable-lpd=false",
  ]);
  start("application", join(BIN, "torrplay"), [
    "--no-dht",
    "--listen",
    `${HOST}:${APPLICATION_PORT}`,
    "--peer-port",
    String(PEER_PORT),
    "--data-dir",
    join(work, "data"),
  ]);

  const deadline = Date.now() + READY_MS;
  for (const [what, isUp] of [
    ["the tracker", () => accepts(TRACKER_PORT)],
    ["the seeder", () => accepts(SEEDER_PORT)],
    [`torrplay in ${BIN}`, applicationReady],
  ] as const) {
    while (!(await isUp())) {
      if (lost !== undefined || Date.now() > deadline) {
        await stop();
        throw new Error(lost ?? `${what} did not become ready; see ${LOGS}`);
      }
      await new Promise((resolve) => setTimeout(resolve, 100));
    }
  }
  return stop;
}
