//! Local load observations for diagnostics; never change command deadlines.

#[derive(Debug, Clone, Copy)]
pub struct SystemLoad {
    pub load1: f64,
    pub cpus: usize,
}

impl SystemLoad {
    pub fn overloaded(self) -> bool {
        self.cpus > 0 && self.load1.is_finite() && self.load1 > 2.0 * self.cpus as f64
    }

    pub fn description(self) -> String {
        format!("System load is {:.1} on {} CPUs", self.load1, self.cpus)
    }

    pub fn timeout_hint(self, error: &str) -> Option<String> {
        let lower = error.to_ascii_lowercase();
        if self.overloaded()
            && (lower.contains("relay") || lower.contains("cdp"))
            && (lower.contains("timeout") || lower.contains("timed out"))
        {
            Some(format!("{}; relay commands slow down sharply under load — this may not be a chrome-use fault.", self.description()))
        } else {
            None
        }
    }
}

/// getloadavg is available on macOS/Linux; unsupported platforms omit the hint.
pub fn current() -> Option<SystemLoad> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let mut load1 = 0.0;
        // SAFETY: getloadavg receives a valid buffer for the one requested double.
        if unsafe { libc::getloadavg(&mut load1, 1) } != 1 {
            return None;
        }
        let cpus = std::thread::available_parallelism().ok()?.get();
        Some(SystemLoad { load1, cpus })
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    None
}

pub fn annotate_timeout(error: String) -> String {
    match current().and_then(|load| load.timeout_hint(&error)) {
        Some(hint) => format!("{error}\n{hint}"),
        None => error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn threshold_is_strict_and_hint_is_scoped_to_relay_cdp_timeouts() {
        for load1 in [0.0, 19.9, 20.0, f64::NAN] {
            assert!(SystemLoad { load1, cpus: 10 }
                .timeout_hint("relay timeout")
                .is_none());
        }
        let load = SystemLoad {
            load1: 250.4,
            cpus: 10,
        };
        assert_eq!(load.timeout_hint("CDP command timed out after 30s: Runtime.evaluate").unwrap(),
            "System load is 250.4 on 10 CPUs; relay commands slow down sharply under load — this may not be a chrome-use fault.");
        assert!(load.timeout_hint("relay timeout after 8000ms").is_some());
        assert!(load.timeout_hint("navigation timeout").is_none());
        assert!(load.timeout_hint("CDP connection closed").is_none());
        assert!(!SystemLoad {
            load1: 250.0,
            cpus: 0
        }
        .overloaded());
    }
}
