// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

// Draws the demo's posters from plain shapes, and writes them as images to
// public/demo-media/posters. Their titles are invented, so the art is ours and
// needs no third-party license. Run it with `pnpm demo-posters` after changing
// a scene, and update POSTER_BYTES in src/lib/demo/fixtures.ts with what it
// prints. It renders through the installed Chrome, as the browser tests do.

import { chromium } from "@playwright/test";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const ROOT = join(import.meta.dirname, "..");
const OUTPUT = join(ROOT, "public/demo-media/posters");
const FONTS = join(ROOT, "node_modules/@fontsource-variable/onest/files");
const WIDTH = 600;
const HEIGHT = 900;
const WEBP_QUALITY = 0.8;

/** A repeatable stream of numbers from 0 to 1, so the art does not change
 * between runs. */
function seeded(seed) {
  let state = seed;
  return () => {
    state = (state + 0x6d2b79f5) | 0;
    let mixed = Math.imul(state ^ (state >>> 15), 1 | state);
    mixed = (mixed + Math.imul(mixed ^ (mixed >>> 7), 61 | mixed)) ^ mixed;
    return ((mixed ^ (mixed >>> 14)) >>> 0) / 4294967296;
  };
}

const between = (random, low, high) => low + random() * (high - low);
const pick = (random, choices) => choices[Math.floor(random() * choices.length)];

function wrap(body, defs = "") {
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${WIDTH} ${HEIGHT}" width="${WIDTH}" height="${HEIGHT}"><defs>${defs}</defs>${body}</svg>`;
}

function text(x, y, content, size, fill, weight = 700, anchor = "start", spacing = 0) {
  return `<text x="${x}" y="${y}" font-family="Onest" font-size="${size}" font-weight="${weight}" fill="${fill}" text-anchor="${anchor}" letter-spacing="${spacing}">${content}</text>`;
}

function stars(seed, count, top, bottom, fill) {
  const random = seeded(seed);
  return Array.from(
    { length: count },
    () =>
      `<circle cx="${between(random, 10, 590).toFixed(0)}" cy="${between(random, top, bottom).toFixed(0)}" r="${pick(random, [0.9, 1.2, 1.6, 2.2])}" fill="${fill}" opacity="${between(random, 0.35, 1).toFixed(2)}"/>`,
  ).join("");
}

function gear(cx, cy, outer, inner, hole, teeth, fill, holeFill, turn = 0) {
  const points = Array.from({ length: teeth * 4 }, (_, tooth) => {
    const angle = (turn * Math.PI) / 180 + (2 * Math.PI * tooth) / (teeth * 4);
    const radius = tooth % 4 < 2 ? outer : inner;
    return `${(cx + radius * Math.cos(angle)).toFixed(1)},${(cy + radius * Math.sin(angle)).toFixed(1)}`;
  });
  return `<polygon points="${points.join(" ")}" fill="${fill}"/><circle cx="${cx}" cy="${cy}" r="${hole}" fill="${holeFill}"/>`;
}

function theLighthouseKeepers() {
  return wrap(
    `
<rect width="600" height="900" fill="url(#lk-sky)"/>
${stars(11, 46, 20, 330, "#CFE6EE")}
<polygon points="212,398 620,270 620,500" fill="url(#lk-beam)"/>
<polygon points="196,398 -20,330 -20,440" fill="url(#lk-beam)" opacity=".35"
  transform="translate(0,0)"/>
<ellipse cx="300" cy="640" rx="430" ry="70" fill="#8FC3C6" opacity=".30" filter="url(#lk-soft)"/>
<ellipse cx="120" cy="560" rx="260" ry="46" fill="#8FC3C6" opacity=".20" filter="url(#lk-soft)"/>
<circle cx="520" cy="566" r="16" fill="#F472B6" opacity=".75" filter="url(#lk-glow)"/>
<circle cx="520" cy="566" r="4.5" fill="#FFD6E8"/>
<path d="M0 900V735c62-20 104-46 150-56 40-9 66 4 98-4 34-9 60-30 104-22 44 8 62 44 112 52 50 8 92-14 136-6v201z"
  fill="#06121C"/>
<polygon points="166,690 178,420 226,420 238,690" fill="#E9EDE6"/>
<polygon points="172.6,552 175.4,492 228.6,492 231.4,552" fill="#C2483F"/>
<polygon points="168,640 170.4,590 233.6,590 236,640" fill="#C2483F"/>
<rect x="168" y="408" width="68" height="13" rx="2" fill="#0B1C28"/>
<rect x="179" y="366" width="46" height="42" fill="#FFE9A8"/>
<rect x="179" y="366" width="46" height="42" fill="none" stroke="#0B1C28" stroke-width="5"/>
<line x1="202" y1="366" x2="202" y2="408" stroke="#0B1C28" stroke-width="3"/>
<path d="M172 366 L202 334 L232 366Z" fill="#0B1C28"/>
<rect x="196" y="645" width="14" height="45" rx="7" fill="#0B1C28"/>
<path d="M0 815c78-16 118 8 196-2s118-30 204-16 124 22 200 6v97H0z" fill="#0A2230"/>
${text(44, 104, "The Lighthouse", 62, "#F4F1E4", 700, "start", -1)}
${text(44, 174, "Keepers", 62, "#F4F1E4", 700, "start", -1)}
${text(46, 850, "A SERIES", 18, "#8FC3C6", 600, "start", 5)}
`,
    `
<linearGradient id="lk-sky" x1="0" y1="0" x2="0" y2="1">
  <stop offset="0" stop-color="#071421"/><stop offset=".55" stop-color="#0F3244"/>
  <stop offset="1" stop-color="#1F5A63"/></linearGradient>
<linearGradient id="lk-beam" x1="0" y1="0" x2="1" y2="0">
  <stop offset="0" stop-color="#FFE9A8" stop-opacity=".85"/>
  <stop offset="1" stop-color="#FFE9A8" stop-opacity="0"/></linearGradient>
<filter id="lk-soft" x="-30%" y="-30%" width="160%" height="160%">
  <feGaussianBlur stdDeviation="16"/></filter>
<filter id="lk-glow" x="-200%" y="-200%" width="500%" height="500%">
  <feGaussianBlur stdDeviation="9"/></filter>`,
  );
}

function paperMoons() {
  const lines = [
    [44, 668, 240],
    [44, 684, 226],
    [44, 700, 240],
    [44, 716, 198],
    [316, 668, 240],
    [316, 684, 240],
    [316, 700, 212],
    [316, 716, 232],
  ]
    .map(
      ([x, y, width]) =>
        `<rect x="${x}" y="${y}" width="${width}" height="6" rx="3" fill="#1F2A44" opacity=".22"/>`,
    )
    .join("");
  const phases = [
    [84, "#F7F1E0"],
    [192, "#EBCB73"],
    [300, "#E2B33F"],
    [408, "#D0602F"],
    [516, "#1F2A44"],
  ]
    .map(([x, fill]) => `<circle cx="${x}" cy="588" r="26" fill="${fill}" filter="url(#pm-lift)"/>`)
    .join("");
  return wrap(
    `
<rect width="600" height="900" fill="#EADFC8"/>
<circle cx="300" cy="310" r="190" fill="#1F2A44" filter="url(#pm-lift)"/>
<g clip-path="url(#pm-big)">${stars(23, 40, 120, 500, "#F7F1E0")}</g>
<circle cx="266" cy="310" r="150" fill="#E2B33F" mask="url(#pm-crescent)" filter="url(#pm-lift)"/>
<rect x="44" y="636" width="512" height="5" fill="#1F2A44"/>
${phases}
${lines}
${text(44, 104, "THE DAILY", 18, "#1F2A44", 600, "start", 5)}
${text(556, 104, "TOMORROW’S EDITION", 18, "#1F2A44", 600, "end", 3)}
<rect x="44" y="118" width="512" height="3" fill="#1F2A44"/>
${text(42, 824, "Paper Moons", 76, "#1F2A44", 700, "start", -2)}
`,
    `
<filter id="pm-lift" x="-20%" y="-20%" width="140%" height="140%">
  <feDropShadow dx="5" dy="8" stdDeviation="5" flood-color="#4A3A20" flood-opacity=".38"/>
</filter>
<mask id="pm-crescent"><rect width="600" height="900" fill="#fff"/>
  <circle cx="376" cy="282" r="150" fill="#000"/></mask>
<clipPath id="pm-big"><circle cx="300" cy="310" r="190"/></clipPath>`,
  );
}

function dolgayaZima() {
  return wrap(
    `
<rect width="600" height="900" fill="url(#dz-sky)"/>
${stars(37, 90, 10, 560, "#E6EEFF")}
<path d="M-40 150C80 240 190 120 310 200S520 250 640 130V470C520 560 410 460 290 520S70 560-40 470Z"
  fill="url(#dz-a1)" filter="url(#dz-blur)"/>
<path d="M-40 260C110 330 220 240 340 320S540 330 640 260V540C500 610 400 540 280 590S80 620-40 560Z"
  fill="url(#dz-a2)" filter="url(#dz-blur)"/>
<path d="M0 900V690c110-38 220-52 330-30s190 18 270-8v248z" fill="url(#dz-snow)"/>
<path d="M0 900V770c150-30 260-6 360 4s170-4 240-26v152z" fill="#DDE7F7" opacity=".55"/>
<line x1="372" y1="664" x2="372" y2="478" stroke="#0B1533" stroke-width="5"/>
<line x1="348" y1="500" x2="396" y2="500" stroke="#0B1533" stroke-width="4"/>
<circle cx="348" cy="494" r="7" fill="#0B1533"/><circle cx="396" cy="494" r="7" fill="#0B1533"/>
<circle cx="372" cy="476" r="4" fill="#F472B6"/>
<circle cx="372" cy="476" r="10" fill="#F472B6" opacity=".55" filter="url(#dz-glow)"/>
<rect x="226" y="618" width="118" height="62" fill="#0B1533"/>
<path d="M214 620 L285 572 L356 620Z" fill="#0B1533"/>
<path d="M214 620 L285 572 L356 620 L340 620 L285 584 L230 620Z" fill="#E8F0FF"/>
<rect x="282" y="634" width="30" height="26" fill="#FFC56B"/>
<rect x="282" y="634" width="30" height="26" fill="#FFC56B" opacity=".8" filter="url(#dz-glow)"/>
<line x1="297" y1="634" x2="297" y2="660" stroke="#0B1533" stroke-width="2.5"/>
<rect x="246" y="640" width="20" height="40" fill="#1B2A55"/>
${text(44, 106, "Долгая", 78, "#F2F6FF", 700, "start", -1)}
${text(44, 188, "зима", 78, "#F2F6FF", 700, "start", -1)}
${text(46, 850, "ФИЛЬМ · 2020", 18, "#0B1533", 600, "start", 4)}
`,
    `
<linearGradient id="dz-sky" x1="0" y1="0" x2="0" y2="1">
  <stop offset="0" stop-color="#050A1E"/><stop offset=".7" stop-color="#14224A"/>
  <stop offset="1" stop-color="#2A3F73"/></linearGradient>
<linearGradient id="dz-a1" x1="0" y1="0" x2="0" y2="1">
  <stop offset="0" stop-color="#3EE6B0" stop-opacity="0"/>
  <stop offset=".65" stop-color="#3EE6B0" stop-opacity=".55"/>
  <stop offset="1" stop-color="#7DF0FF" stop-opacity="0"/></linearGradient>
<linearGradient id="dz-a2" x1="0" y1="0" x2="0" y2="1">
  <stop offset="0" stop-color="#A78BFA" stop-opacity="0"/>
  <stop offset=".7" stop-color="#7C9CFA" stop-opacity=".45"/>
  <stop offset="1" stop-color="#3EE6B0" stop-opacity="0"/></linearGradient>
<linearGradient id="dz-snow" x1="0" y1="0" x2="0" y2="1">
  <stop offset="0" stop-color="#B9CBEA"/><stop offset="1" stop-color="#5D74A8"/></linearGradient>
<filter id="dz-blur" x="-20%" y="-20%" width="140%" height="140%">
  <feGaussianBlur stdDeviation="20"/></filter>
<filter id="dz-glow" x="-200%" y="-200%" width="500%" height="500%">
  <feGaussianBlur stdDeviation="10"/></filter>`,
  );
}

function saltAndCopper() {
  const rings = [
    [196, 3, 0.5],
    [162, 2, 0.4],
    [116, 2, 0.35],
    [64, 2, 0.3],
  ]
    .map(
      ([radius, width, opacity]) =>
        `<circle cx="300" cy="450" r="${radius}" fill="none" stroke="#3D1A0E" stroke-width="${width}" opacity="${opacity}"/>`,
    )
    .join("");
  const random = seeded(53);
  const crystals = Array.from({ length: 46 }, () => {
    const x = between(random, 0, 600).toFixed(0);
    const y = between(random, 640, 900).toFixed(0);
    const size = between(random, 5, 26).toFixed(0);
    const turn = between(random, 0, 90).toFixed(0);
    const shade = pick(random, ["#FFFFFF", "#F2EEE6", "#D9D3C8", "#BEB7AA"]);
    return `<rect x="${x}" y="${y}" width="${size}" height="${size}" fill="${shade}" transform="rotate(${turn} ${x} ${y})"/>`;
  }).join("");
  return wrap(
    `
<rect width="600" height="900" fill="url(#sc-bg)"/>
<g clip-path="url(#sc-upper)"><circle cx="300" cy="450" r="220" fill="url(#sc-disc)"/>${rings}</g>
<g clip-path="url(#sc-lower)" transform="translate(14,22)">
  <circle cx="300" cy="450" r="220" fill="url(#sc-disc)"/>${rings}</g>
<path d="M0 900V716l90-30 70 22 110-42 86 34 100-38 144 44v194z" fill="#E8E2D6"/>
${crystals}
${text(44, 102, "SALT", 84, "#FBF6EC", 700, "start", 2)}
${text(44, 172, "&amp; COPPER", 56, "#F5B070", 700, "start", 2)}
`,
    `
<linearGradient id="sc-bg" x1="0" y1="0" x2="1" y2="1">
  <stop offset="0" stop-color="#1A0B07"/><stop offset="1" stop-color="#3D1A0E"/></linearGradient>
<radialGradient id="sc-disc" cx=".36" cy=".3" r=".85">
  <stop offset="0" stop-color="#F5B070"/><stop offset=".45" stop-color="#C8682C"/>
  <stop offset="1" stop-color="#5E2A10"/></radialGradient>
<clipPath id="sc-upper"><polygon points="0,0 600,0 600,296 0,616"/></clipPath>
<clipPath id="sc-lower"><polygon points="0,616 600,296 600,900 0,900"/></clipPath>`,
  );
}

function glassHarbor() {
  const rays = [
    [40, 44, -70],
    [170, 70, -30],
    [330, 40, 26],
    [450, 80, 80],
  ]
    .map(
      ([x, width, lean]) =>
        `<polygon points="${x},0 ${x + width},0 ${x + width + lean},640 ${x + lean},640" fill="url(#gh-ray)"/>`,
    )
    .join("");
  const random = seeded(71);
  const bubbles = Array.from(
    { length: 8 },
    (_, index) =>
      `<circle cx="${(392 + between(random, -16, 26)).toFixed(0)}" cy="${240 + index * 22}" r="${between(random, 2, 6).toFixed(1)}" fill="none" stroke="#D8FFF7" stroke-width="1.6" opacity=".8"/>`,
  ).join("");
  const town = `
<path d="M0 900V742h44v-46l34-30 34 30v46h40v-74l40-34 40 34v74h34V600l22-64 22 64v142h36v-58l38-32
38 32v58h44v-92l36-30 36 30v92h62v158z" fill="#052130"/>`;
  const windows = [
    [72, 712, 0.9],
    [186, 694, 0.55],
    [214, 694, 0.9],
    [334, 712, 0.7],
    [458, 690, 0.9],
    [486, 690, 0.45],
    [292, 660, 0.8],
  ]
    .map(
      ([x, y, opacity]) =>
        `<rect x="${x}" y="${y}" width="11" height="15" fill="#F6D66B" opacity="${opacity}"/>`,
    )
    .join("");
  return wrap(
    `
<rect width="600" height="900" fill="url(#gh-sea)"/>
${rays}
<g filter="url(#gh-soft)" opacity=".55" transform="translate(0,-56) scale(1,.95)">${town}</g>
${town}
${windows}
<g transform="translate(372 404) rotate(-24)" fill="#062433">
  <ellipse cx="0" cy="0" rx="17" ry="40"/><circle cx="0" cy="-54" r="15"/>
  <rect x="-27" y="-30" width="11" height="42" rx="5"/>
  <path d="M-11 30 L-24 104 L-8 104 L2 42Z"/><path d="M11 30 L24 100 L8 100 L0 42Z"/>
  <path d="M-30 100 L-4 100 L-14 134Z"/><path d="M4 96 L30 96 L24 130Z"/>
</g>
<rect x="374" y="340" width="9" height="9" rx="2" fill="#F6D66B" transform="rotate(-24 378 345)"/>
${bubbles}
${text(44, 106, "Glass", 84, "#F2FFFB", 700, "start", -1)}
${text(44, 190, "Harbor", 84, "#F2FFFB", 700, "start", -1)}
`,
    `
<linearGradient id="gh-sea" x1="0" y1="0" x2="0" y2="1">
  <stop offset="0" stop-color="#2F96A2"/><stop offset=".3" stop-color="#1E8A96"/>
  <stop offset=".7" stop-color="#0C4A5E"/><stop offset="1" stop-color="#062433"/></linearGradient>
<linearGradient id="gh-ray" x1="0" y1="0" x2="0" y2="1">
  <stop offset="0" stop-color="#E9FFF8" stop-opacity=".3"/>
  <stop offset="1" stop-color="#E9FFF8" stop-opacity="0"/></linearGradient>
<filter id="gh-soft" x="-20%" y="-20%" width="140%" height="140%">
  <feGaussianBlur stdDeviation="3"/></filter>`,
  );
}

function smallEngines() {
  const ink = "#20242C";
  const ground = "#E7B23C";
  const red = "#C9432E";
  const line = (x1, y1, x2, y2) =>
    `<line x1="${x1}" y1="${y1}" x2="${x2}" y2="${y2}" stroke="${ink}" stroke-width="1" opacity=".12"/>`;
  const grid = [
    ...Array.from({ length: 13 }, (_, index) => line(index * 50, 0, index * 50, 900)),
    ...Array.from({ length: 19 }, (_, index) => line(0, index * 50, 600, index * 50)),
  ].join("");
  return wrap(`
<rect width="600" height="900" fill="${ground}"/>
${grid}
${gear(236, 330, 178, 148, 44, 16, ink, ground, 5)}
<circle cx="236" cy="330" r="96" fill="none" stroke="${ground}" stroke-width="4"/>
${gear(437, 198, 92, 70, 22, 9, red, ground, 12)}
${gear(462, 450, 62, 46, 15, 7, ink, ground, 20)}
<rect x="44" y="812" width="512" height="5" fill="${ink}"/>
${text(44, 700, "Small", 84, ink, 700, "start", -1)}
${text(44, 782, "Engines", 84, ink, 700, "start", -1)}
${text(46, 858, "A DOCUMENTARY", 18, ink, 600, "start", 5)}
`);
}

const POSTERS = {
  "the-lighthouse-keepers": theLighthouseKeepers,
  "paper-moons": paperMoons,
  "glass-harbor": glassHarbor,
  "salt-and-copper": saltAndCopper,
  "dolgaya-zima": dolgayaZima,
  "small-engines": smallEngines,
};

const faces = ["latin", "cyrillic"]
  .map((subset) => {
    const font = readFileSync(join(FONTS, `onest-${subset}-wght-normal.woff2`)).toString("base64");
    return `@font-face{font-family:Onest;font-weight:100 900;src:url(data:font/woff2;base64,${font}) format("woff2")}`;
  })
  .join("");

mkdirSync(OUTPUT, { recursive: true });
const browser = await chromium.launch({ channel: "chrome" });
const page = await browser.newPage({ viewport: { width: WIDTH, height: HEIGHT } });
for (const [name, draw] of Object.entries(POSTERS)) {
  await page.setContent(`<style>${faces}body{margin:0}</style>${draw()}`);
  await page.evaluate(() =>
    Promise.all([document.fonts.load("700 20px Onest", "Aя"), document.fonts.ready]),
  );
  const screenshot = (await page.screenshot({ type: "png" })).toString("base64");
  const encoded = await page.evaluate(
    async ([png, quality]) => {
      const image = new Image();
      image.src = `data:image/png;base64,${png}`;
      await image.decode();
      const canvas = document.createElement("canvas");
      canvas.width = image.width;
      canvas.height = image.height;
      canvas.getContext("2d").drawImage(image, 0, 0);
      return canvas.toDataURL("image/webp", quality).split(",")[1];
    },
    [screenshot, WEBP_QUALITY],
  );
  const bytes = Buffer.from(encoded, "base64");
  writeFileSync(join(OUTPUT, `${name}.webp`), bytes);
  console.log(`  "${name}": ${bytes.length},`);
}
await browser.close();
