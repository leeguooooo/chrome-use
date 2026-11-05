// Minimal PNG decoder for marker acceptance: counts pixels matching a colour
// predicate and returns their bounds and centroid. 8-bit RGB/RGBA only.
import { inflateSync } from 'node:zlib';

export function decodePng(png) {
  let offset = 8, width, height, channels; const idat = [];
  while (offset < png.length) {
    const size = png.readUInt32BE(offset), type = png.toString('ascii', offset + 4, offset + 8);
    const data = png.subarray(offset + 8, offset + 8 + size); offset += size + 12;
    if (type === 'IHDR') {
      width = data.readUInt32BE(0); height = data.readUInt32BE(4);
      if (data[8] !== 8 || ![2, 6].includes(data[9]) || data[12] !== 0) throw Error('Unsupported PNG');
      channels = data[9] === 2 ? 3 : 4;
    }
    if (type === 'IDAT') idat.push(data);
  }
  const raw = inflateSync(Buffer.concat(idat)); const pixels = Buffer.alloc(width * height * channels);
  const paeth = (a, b, c) => { const p = a + b - c, pa = Math.abs(p - a), pb = Math.abs(p - b), pc = Math.abs(p - c); return pa <= pb && pa <= pc ? a : pb <= pc ? b : c; };
  let cursor = 0, previous = Buffer.alloc(width * channels);
  for (let y = 0; y < height; y++) {
    const filter = raw[cursor++], row = Buffer.from(raw.subarray(cursor, cursor + width * channels)); cursor += row.length;
    for (let x = 0; x < row.length; x++) {
      const a = x >= channels ? row[x - channels] : 0, b = previous[x], c = x >= channels ? previous[x - channels] : 0;
      const pr = filter === 0 ? 0 : filter === 1 ? a : filter === 2 ? b : filter === 3 ? (a + b) >> 1 : filter === 4 ? paeth(a, b, c) : NaN;
      if (!Number.isFinite(pr)) throw Error('Bad PNG filter'); row[x] = (row[x] + pr) & 255;
    }
    row.copy(pixels, y * width * channels); previous = row;
  }
  return { width, height, channels, pixels };
}

export function region(img, match) {
  let count = 0, sx = 0, sy = 0; const b = [img.width, img.height, -1, -1];
  for (let y = 0; y < img.height; y++) for (let x = 0; x < img.width; x++) {
    const i = (y * img.width + x) * img.channels;
    if (match(img.pixels[i], img.pixels[i + 1], img.pixels[i + 2])) {
      count++; sx += x; sy += y;
      if (x < b[0]) b[0] = x; if (y < b[1]) b[1] = y; if (x > b[2]) b[2] = x; if (y > b[3]) b[3] = y;
    }
  }
  return count ? { count, bounds: b, center: [sx / count, sy / count] } : { count: 0 };
}

export const magenta = (r, g, b) => r > 200 && g < 100 && b > 200;
export const green = (r, g, b) => g > 180 && r < 170 && b < 170 && g - r > 60;
