# Changelog

## 1.5.101

<!-- release:start -->
### Bug Fixes

- **File upload confirmation:** React dropzones that consume and clear or replace their file input now return success with a verification warning instead of a false rejection (#208).
- **Controlled selects:** Native selects use platform setters and dispatch both input and change events so controlled forms commit the selection (#209).
- **Idle session preservation:** Idle daemon recycling preserves external Chrome tabs, URLs and in-page state. Explicit close and session stop still clean up created tabs (#210).
- **Relay navigation:** ab-connect 0.5.20 uses browser-level navigation first and reports rapid same-URL reload loops with recovery guidance. App Store Connect still needs a signed-in site regression check (#211).

### Improvements

- Added browser-backed upload and select regression tests and extension navigation/reload-loop tests.
- Updated command help, agent guidance, and English/Chinese documentation.

### Contributors

- @leeguooooo
<!-- release:end -->

Earlier releases are documented in [the English changelog](docs/en/changelog.html) and [the Chinese changelog](docs/changelog.html).
