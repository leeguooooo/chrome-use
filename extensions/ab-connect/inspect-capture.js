// Inspect & Ask screenshots. chrome.tabs.captureVisibleTab shoots whatever tab
// is active in a window, not a tab id, so a capture only counts when the same
// tab, document and layout are confirmed before and after it. Anything else
// drops the screenshots with a stated reason; the extension never switches
// the user's tabs to make a capture succeed.

import { t } from './i18n.js';

export const MAX_CROP_SIDE = 1600;

export function layoutsMatch(a, b, tolerance = 1) {
  if (!a || !b) return false;
  for (const key of ['innerWidth', 'innerHeight', 'scrollX', 'scrollY', 'devicePixelRatio']) {
    if (typeof a[key] !== 'number' || typeof b[key] !== 'number' || Math.abs(a[key] - b[key]) > (key === 'devicePixelRatio' ? 0.001 : tolerance)) return false;
  }
  if (!Array.isArray(a.rects) || !Array.isArray(b.rects) || a.rects.length !== b.rects.length) return false;
  return a.rects.every((r, i) => ['x', 'y', 'width', 'height'].every(k => Math.abs(r[k] - b.rects[i][k]) <= tolerance));
}

// Bitmap pixels per CSS pixel come from the bitmap itself, not devicePixelRatio
// (zoom and display scaling make them differ).
export function cropPlan({ bitmapWidth, bitmapHeight, layout, maxSide = MAX_CROP_SIDE }) {
  if (!(bitmapWidth > 0 && bitmapHeight > 0 && layout?.innerWidth > 0 && layout?.innerHeight > 0)) return { ok: false, reason: t('inspect_capture_size_unknown') };
  const sx = bitmapWidth / layout.innerWidth;
  const sy = bitmapHeight / layout.innerHeight;
  if (Math.abs(sx - sy) > 0.02) return { ok: false, reason: t('inspect_capture_proportions') };
  const crops = layout.rects.map(rect => {
    const x0 = Math.max(0, Math.floor(rect.x * sx));
    const y0 = Math.max(0, Math.floor(rect.y * sy));
    const x1 = Math.min(bitmapWidth, Math.ceil((rect.x + rect.width) * sx));
    const y1 = Math.min(bitmapHeight, Math.ceil((rect.y + rect.height) * sy));
    if (x1 - x0 < 2 || y1 - y0 < 2) return null;
    const sw = x1 - x0;
    const sh = y1 - y0;
    const scale = Math.min(1, maxSide / Math.max(sw, sh));
    return { sx: x0, sy: y0, sw, sh, dw: Math.max(1, Math.round(sw * scale)), dh: Math.max(1, Math.round(sh * scale)) };
  });
  return { ok: true, scale: sx, crops };
}

// Pre/post checks around one capture. `before`/`after` are tab snapshots and
// generations taken by the background; layouts come from the picker.
export function captureVerdict({ before, after, generationBefore, generationAfter, layoutBefore, layoutAfter, topDocumentId, documentAfter }) {
  if (!before?.active) return { ok: false, reason: t('inspect_capture_needs_visible_tab') };
  if (!after?.active || after.windowId !== before.windowId || after.id !== before.id) return { ok: false, reason: t('inspect_capture_tab_changed') };
  if (generationAfter !== generationBefore) return { ok: false, reason: t('inspect_capture_zoom_changed') };
  if (documentAfter !== topDocumentId) return { ok: false, reason: t('inspect_capture_navigated') };
  if (!layoutsMatch(layoutBefore, layoutAfter)) return { ok: false, reason: t('inspect_capture_moved') };
  return { ok: true };
}

// Runs in the service worker. deps: {captureVisibleTab, fetchBlob} are injected
// for tests; OffscreenCanvas/createImageBitmap are service-worker globals.
export async function cropScreenshots(dataUrl, layout) {
  const blob = await (await fetch(dataUrl)).blob();
  const bitmap = await createImageBitmap(blob);
  try {
    const plan = cropPlan({ bitmapWidth: bitmap.width, bitmapHeight: bitmap.height, layout });
    if (!plan.ok) return { ok: false, reason: plan.reason, images: [] };
    const images = [];
    for (const crop of plan.crops) {
      if (!crop) { images.push(null); continue; }
      const canvas = new OffscreenCanvas(crop.dw, crop.dh);
      canvas.getContext('2d').drawImage(bitmap, crop.sx, crop.sy, crop.sw, crop.sh, 0, 0, crop.dw, crop.dh);
      const png = await canvas.convertToBlob({ type: 'image/png' });
      images.push(await blobToDataUrl(png));
    }
    return { ok: true, images };
  } finally {
    bitmap.close?.();
  }
}

async function blobToDataUrl(blob) {
  const bytes = new Uint8Array(await blob.arrayBuffer());
  let binary = '';
  for (let i = 0; i < bytes.length; i += 0x8000) binary += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return 'data:image/png;base64,' + btoa(binary);
}
