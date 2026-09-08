import { readFileSync } from 'node:fs';
import { pathToFileURL } from 'node:url';

// A release must publish the reviewed notes for the exact packaged version.
export function releaseNotes(changelog, version, tag) {
  if (tag !== `v${version}`) throw new Error(`Tag ${tag} does not match package version ${version}`);
  const start = '<!-- release:start -->';
  const end = '<!-- release:end -->';
  if (changelog.split(start).length !== 2 || changelog.split(end).length !== 2) {
    throw new Error('Expected exactly one release marker pair');
  }
  const from = changelog.indexOf(start);
  const to = changelog.indexOf(end);
  if (to <= from) throw new Error('Release markers are out of order');
  const headings = [...changelog.slice(0, from).matchAll(/^## ([^\r\n]+)$/gm)];
  if (headings.length !== 1 || headings[0][1] !== version) {
    throw new Error('Release markers must belong to the first version heading');
  }
  const notes = changelog.slice(from + start.length, to).trim();
  if (!notes || /^## /m.test(notes)) throw new Error('Release notes are empty or span multiple versions');
  return notes + '\n';
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    const { version } = JSON.parse(readFileSync('package.json', 'utf8'));
    process.stdout.write(releaseNotes(readFileSync('CHANGELOG.md', 'utf8'), version, process.argv[2]));
  } catch (error) {
    console.error(`Release notes: ${error.message}`);
    process.exitCode = 1;
  }
}
