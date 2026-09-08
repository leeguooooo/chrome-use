import { test } from 'node:test';
import assert from 'node:assert/strict';
import { releaseNotes } from './release-notes.js';

const notes = '# Changelog\n\n## 1.2.3\n<!-- release:start -->\n### Bug Fixes\n\n- Preserve action results.\n<!-- release:end -->\n## 1.2.2\nOld notes.\n';
test('publishes only reviewed notes for the exact tag', () => {
  assert.equal(releaseNotes(notes, '1.2.3', 'v1.2.3'), '### Bug Fixes\n\n- Preserve action results.\n');
});
test('rejects mismatched tag including prerelease suffix', () => {
  for (const tag of ['v1.2.2', 'v1.2.3-rc.1', undefined]) {
    assert.throws(() => releaseNotes(notes, '1.2.3', tag), /does not match/);
  }
});
test('rejects missing, duplicate and reversed markers', () => {
  for (const invalid of [notes.replace('<!-- release:end -->', ''), notes + '<!-- release:start -->', notes.replace('release:start', 'TEMP').replace('release:end', 'release:start').replace('TEMP', 'release:end')]) {
    assert.throws(() => releaseNotes(invalid, '1.2.3', 'v1.2.3'), /marker/i);
  }
});
test('rejects notes for an older heading or across versions', () => {
  for (const invalid of [notes.replace('## 1.2.3', '## 1.2.4\n## 1.2.3'), notes.replace('### Bug Fixes', '## 1.2.2'), notes.replace('## 1.2.3', '## 1.2.2')]) {
    assert.throws(() => releaseNotes(invalid, '1.2.3', 'v1.2.3'));
  }
});
test('rejects empty notes', () => {
  assert.throws(() => releaseNotes('## 1.2.3\n<!-- release:start -->\n<!-- release:end -->', '1.2.3', 'v1.2.3'), /empty/);
});
