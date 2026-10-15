import './test-i18n.js';
import assert from 'node:assert/strict';
import test from 'node:test';
import { layoutsMatch, cropPlan, captureVerdict } from './inspect-capture.js';

const layout = { innerWidth: 1000, innerHeight: 500, scrollX: 0, scrollY: 40, devicePixelRatio: 2, rects: [{ x: 10, y: 20, width: 100, height: 50 }, { x: 990, y: 490, width: 50, height: 50 }] };

test('layouts compare viewport, scroll and every rect within 1px', () => {
  assert.equal(layoutsMatch(layout, structuredClone(layout)), true);
  assert.equal(layoutsMatch(layout, { ...layout, scrollY: 42 }), false);
  assert.equal(layoutsMatch(layout, { ...layout, rects: [layout.rects[0]] }), false);
  const moved = structuredClone(layout); moved.rects[0].x += 3;
  assert.equal(layoutsMatch(layout, moved), false);
});

test('crop scale comes from the bitmap and is clamped to it', () => {
  const plan = cropPlan({ bitmapWidth: 1500, bitmapHeight: 750, layout });
  assert.equal(plan.ok, true);
  assert.equal(plan.scale, 1.5);
  assert.deepEqual(plan.crops[0], { sx: 15, sy: 30, sw: 150, sh: 75, dw: 150, dh: 75 });
  assert.deepEqual(plan.crops[1], { sx: 1485, sy: 735, sw: 15, sh: 15, dw: 15, dh: 15 });
  assert.equal(cropPlan({ bitmapWidth: 1500, bitmapHeight: 900, layout }).ok, false);
  const big = cropPlan({ bitmapWidth: 4000, bitmapHeight: 2000, layout: { ...layout, rects: [{ x: 0, y: 0, width: 1000, height: 500 }] }, maxSide: 1600 });
  assert.deepEqual([big.crops[0].dw, big.crops[0].dh], [1600, 800]);
});

test('capture verdict rejects tab switches, navigation, zoom and layout drift', () => {
  const base = { before: { id: 1, windowId: 9, active: true, status: 'complete' }, after: { id: 1, windowId: 9, active: true }, generationBefore: 3, generationAfter: 3, layoutBefore: layout, layoutAfter: layout, topDocumentId: 'd', documentAfter: 'd' };
  assert.equal(captureVerdict(base).ok, true);
  assert.equal(captureVerdict({ ...base, before: { ...base.before, active: false } }).ok, false);
  assert.equal(captureVerdict({ ...base, before: { ...base.before, status: 'loading' } }).ok, true, 'a loading favicon does not block capture');
  assert.equal(captureVerdict({ ...base, after: { ...base.after, active: false } }).ok, false);
  assert.equal(captureVerdict({ ...base, generationAfter: 4 }).ok, false);
  assert.equal(captureVerdict({ ...base, documentAfter: 'e' }).ok, false);
  assert.equal(captureVerdict({ ...base, layoutAfter: { ...layout, scrollY: 300 } }).ok, false);
});
