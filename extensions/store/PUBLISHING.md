# Publishing ab-connect to the Chrome Web Store

Releases publish themselves. When a new `extensions/ab-connect.zip` lands on
main, `.github/workflows/publish-extension.yml` uploads it and submits it for
review through the Chrome Web Store API (v2). Google still reviews every
version; nobody has to upload anything by hand.

## Every release

1. Bump `"version"` in `extensions/ab-connect/manifest.json`.
2. `sh scripts/pack-extension.sh`
3. Commit the manifest and `extensions/ab-connect.zip`, and merge to main.

CI does the rest. A version the store already has (published or in review) is
skipped. If a publish fails, CI opens an issue labelled `cws-publish`; the next
successful run closes it.

Check the store from a laptop: `python3 scripts/cws.py status`.

## One-time setup (service account)

A service account is used because it does not expire. The earlier OAuth refresh
token came from an app in "Testing" mode; Google expires those after 7 days,
which silently held 0.5.27 to 0.5.29 back.

1. In [Google Cloud Console](https://console.cloud.google.com), pick or create a
   project, then enable **Chrome Web Store API** (APIs & Services > Library).
2. IAM & Admin > Service accounts > **Create service account**. It needs no
   roles. Open it, then Keys > Add key > **JSON**, and keep the downloaded file.
3. In the [Developer Dashboard](https://chrome.google.com/webstore/devconsole),
   go to **Account** and add the service account's email (a publisher can have
   only one).
4. Note the **Publisher ID** under Publisher > Settings.
5. Add two repository secrets:

   ```sh
   gh secret set CWS_SA_KEY -R leeguooooo/chrome-use < path/to/key.json
   gh secret set CWS_PUBLISHER_ID -R leeguooooo/chrome-use --body '<publisher id>'
   ```

6. Optional, for local use: save the key as `.secrets/cws-sa.json` and add
   `CWS_PUBLISHER_ID=...` to `.secrets/cws.env`. Both are git-ignored.

Then run the workflow once by hand (Actions > Publish extension > Run workflow)
or wait for the next zip on main.

The old `CWS_CLIENT_ID` / `CWS_CLIENT_SECRET` / `CWS_REFRESH_TOKEN` secrets still
work as a fallback (`scripts/cws-get-refresh-token.sh` mints a new token) and
can be deleted once the service account works.
