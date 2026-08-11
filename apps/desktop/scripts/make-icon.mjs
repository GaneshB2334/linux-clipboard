// Generate the app icon without an image toolchain (no ImageMagick/rsvg here).
// Emits a 512x512 RGBA PNG: a rounded blue square with a white clipboard glyph.
import { deflateSync } from "node:zlib";
import { writeFileSync, mkdirSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const S = 512;
const px = new Uint8Array(S * S * 4);

const set = (x, y, r, g, b, a) => {
  const i = (y * S + x) * 4;
  // Straight alpha compositing over whatever is already there.
  const sa = a / 255;
  px[i] = Math.round(r * sa + px[i] * (1 - sa));
  px[i + 1] = Math.round(g * sa + px[i + 1] * (1 - sa));
  px[i + 2] = Math.round(b * sa + px[i + 2] * (1 - sa));
  px[i + 3] = Math.max(px[i + 3], a);
};

// Rounded-square background, antialiased via a signed-distance field.
const radius = 112;
const inset = 24;
for (let y = 0; y < S; y++) {
  for (let x = 0; x < S; x++) {
    const dx = Math.max(inset + radius - x, 0, x - (S - inset - radius));
    const dy = Math.max(inset + radius - y, 0, y - (S - inset - radius));
    const d = Math.hypot(dx, dy) - radius;
    const a = Math.max(0, Math.min(1, 0.5 - d));
    if (a > 0) {
      // Vertical gradient, indigo -> blue.
      const t = y / S;
      set(x, y, Math.round(70 + 20 * t), Math.round(90 + 40 * t), 230, Math.round(a * 255));
    }
  }
}

// Clipboard body.
const rect = (x0, y0, x1, y1, r, g, b, rad = 0) => {
  for (let y = y0; y < y1; y++) {
    for (let x = x0; x < x1; x++) {
      if (rad) {
        const dx = Math.max(x0 + rad - x, 0, x - (x1 - rad));
        const dy = Math.max(y0 + rad - y, 0, y - (y1 - rad));
        if (Math.hypot(dx, dy) - rad > 0.5) continue;
      }
      set(x, y, r, g, b, 255);
    }
  }
};

rect(150, 150, 362, 400, 255, 255, 255, 22);
rect(196, 116, 316, 172, 255, 255, 255, 18); // clip at the top
rect(214, 132, 298, 156, 70, 100, 230);      // clip cutout
// Three "lines of content".
rect(186, 226, 326, 244, 150, 170, 220, 9);
rect(186, 272, 326, 290, 150, 170, 220, 9);
rect(186, 318, 272, 336, 150, 170, 220, 9);

// PNG assembly: filter byte 0 per scanline, then a single IDAT.
const raw = Buffer.alloc((S * 4 + 1) * S);
for (let y = 0; y < S; y++) {
  raw[y * (S * 4 + 1)] = 0;
  Buffer.from(px.buffer, y * S * 4, S * 4).copy(raw, y * (S * 4 + 1) + 1);
}

const crcTable = Array.from({ length: 256 }, (_, n) => {
  let c = n;
  for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
  return c >>> 0;
});
const crc32 = (buf) => {
  let c = 0xffffffff;
  for (const b of buf) c = crcTable[(c ^ b) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
};
const chunk = (type, data) => {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length);
  const body = Buffer.concat([Buffer.from(type, "ascii"), data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(body));
  return Buffer.concat([len, body, crc]);
};

const ihdr = Buffer.alloc(13);
ihdr.writeUInt32BE(S, 0);
ihdr.writeUInt32BE(S, 4);
ihdr[8] = 8;  // bit depth
ihdr[9] = 6;  // RGBA
const png = Buffer.concat([
  Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
  chunk("IHDR", ihdr),
  chunk("IDAT", deflateSync(raw, { level: 9 })),
  chunk("IEND", Buffer.alloc(0)),
]);

const out = resolve(dirname(fileURLToPath(import.meta.url)), "../src-tauri/icons/icon.png");
mkdirSync(dirname(out), { recursive: true });
writeFileSync(out, png);
console.log(`wrote ${out} (${png.length} bytes)`);
