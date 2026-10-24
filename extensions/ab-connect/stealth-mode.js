// Stealth mode (on by default): nothing chrome-use adds on its own may be
// visible to the page it drives. Features that draw into the page, such as the
// agent cursor overlay (a main-world global plus a DOM node), only run once
// the user turns stealth mode off on the options page. Kept free of
// `chrome.*` globals so the rule is unit-testable.

export const STEALTH_DEFAULT = true

/** Stealth is on unless the user turned it off explicitly. */
export function stealthOn(s) {
  return !s || s.ab_stealth !== false
}

/** Whether the agent cursor overlay may be drawn into pages. */
export function cursorAllowed(s) {
  return !stealthOn(s) && !!(s && s.ab_cursor)
}
