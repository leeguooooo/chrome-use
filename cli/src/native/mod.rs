#[allow(dead_code)]
pub mod a11y;
#[allow(dead_code)]
pub mod actions;
#[allow(dead_code)]
pub mod adaptive;
#[allow(dead_code)]
pub mod auth;
#[allow(dead_code)]
pub mod browser;
#[allow(dead_code)]
pub mod cdp;
#[allow(dead_code)]
pub mod cookies;
#[allow(dead_code)]
pub mod daemon;
#[allow(dead_code)]
pub mod diff;

pub mod dom_snapshot;
#[allow(dead_code)]
pub mod element;
pub mod first_tab;
#[allow(dead_code)]
pub(crate) mod focus_recovery;
pub mod focus_restore;
#[allow(dead_code)]
pub mod humanize;
#[allow(dead_code)]
pub mod inspect_server;
#[allow(dead_code)]
pub mod interaction;
pub mod login_wall;
mod monaco_model;
#[allow(dead_code)]
pub mod network;
mod observation;
pub(crate) mod panel_action;
mod passkey_guard;
#[allow(dead_code)]
pub mod policy;
pub mod popup_guard;
pub mod progress;
#[allow(dead_code)]
pub mod providers;
pub mod quiet_dom;
#[allow(dead_code)]
pub mod react;
#[allow(dead_code)]
pub mod recording;
pub mod ref_hints;
#[allow(dead_code)]
pub mod relay;
pub(crate) mod relay_targets;
#[allow(dead_code)]
pub mod screenshot;
pub mod semantic_locator;
pub mod sensitive;
pub mod site_policy;

pub mod script;
pub mod teach;

#[allow(dead_code)]
pub mod rotating_slider;
pub mod script_js;
#[allow(dead_code)]
pub mod settle;
pub mod slider;
#[allow(dead_code)]
pub mod snapshot;
#[allow(dead_code)]
pub mod state;
#[allow(dead_code)]
pub mod stealth;
#[allow(dead_code)]
pub mod storage;
#[allow(dead_code)]
pub mod stream;
pub mod timing;
#[allow(dead_code)]
pub mod tracing;
pub mod tracked_requests;
pub mod upload;
pub(crate) mod visibility;
#[allow(dead_code)]
pub mod webdriver;

#[cfg(all(test, feature = "e2e-tests"))]
mod e2e_tests;
#[cfg(test)]
mod parity_tests;

#[cfg(windows)]
mod windows_stdio;
