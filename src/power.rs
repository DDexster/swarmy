//! Whether the machine runs on battery, so the dashboard can slow down and save it.

/// True when a battery reports that it is discharging. Unknown platforms report false.
pub fn on_battery() -> bool {
    #[cfg(target_os = "linux")]
    {
        let Ok(rd) = std::fs::read_dir("/sys/class/power_supply") else {
            return false;
        };
        let mut battery_discharging = false;
        for e in rd.flatten() {
            let p = e.path();
            let kind = std::fs::read_to_string(p.join("type")).unwrap_or_default();
            if kind.trim() == "Mains"
                && std::fs::read_to_string(p.join("online")).is_ok_and(|s| s.trim() == "1")
            {
                return false;
            }
            if kind.trim() == "Battery"
                && std::fs::read_to_string(p.join("status"))
                    .is_ok_and(|s| s.trim() == "Discharging")
            {
                battery_discharging = true;
            }
        }
        battery_discharging
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("pmset")
            .args(["-g", "batt"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains("'Battery Power'"))
            .unwrap_or(false)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        false
    }
}
