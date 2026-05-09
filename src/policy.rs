/// Three-tier bash command safety system.
/// Tier 1 (Open): no confirmation needed
/// Tier 2 (Confirm): requires confirm=true
/// Tier 3 (Destructive): requires allow_destructive=true
/// Rejected: unconditionally blocked

#[derive(Debug, Clone, PartialEq)]
pub enum Tier {
    Open,
    Confirm,
    Destructive,
    Rejected,
}

const TIER2_PATTERNS: &[&str] = &[
    "systemctl",
    "iptables",
    "mount ",
    "umount ",
    "kill ",
    "pkill ",
    "apt-get install",
    "apt-get remove",
    "docker rm ",
    "docker stop ",
    "chown ",
    "chmod 7",
    "chgrp ",
    "crontab ",
    "ufw ",
    "firewall-cmd",
];

const TIER3_PATTERNS: &[&str] = &[
    "rm -rf /",
    "mkfs",
    "of=/dev/",
    "useradd -u 0",
    "passwd ",
    "/etc/shadow",
    "/etc/passwd",
    "/boot/",
];

const REJECTED_PATTERNS: &[&str] = &[
    "dd if=",
    ":(){ :|:& };:",
    "curl | sh",
    "curl | bash",
    "wget | sh",
    "wget | bash",
    "> /dev/sda",
    "> /dev/sdb",
    "> /dev/nvme",
    "rm -rf / ",
    "rm -rf /*",
];

pub fn classify_command(command: &str) -> Tier {
    let lower = command.to_lowercase();

    for pat in REJECTED_PATTERNS {
        if lower.contains(pat) {
            return Tier::Rejected;
        }
    }
    for pat in TIER3_PATTERNS {
        if lower.contains(pat) {
            return Tier::Destructive;
        }
    }
    for pat in TIER2_PATTERNS {
        if lower.contains(pat) {
            return Tier::Confirm;
        }
    }
    Tier::Open
}

pub fn check_command(command: &str, confirm: bool, allow_destructive: bool) -> Result<(), String> {
    match classify_command(command) {
        Tier::Open => Ok(()),
        Tier::Confirm => {
            if confirm {
                Ok(())
            } else {
                Err(format!(
                    "Command requires confirm=true (Tier 2 safety): {}",
                    command
                ))
            }
        }
        Tier::Destructive => {
            if allow_destructive {
                Ok(())
            } else {
                Err(format!(
                    "Command requires allow_destructive=true (Tier 3 safety): {}",
                    command
                ))
            }
        }
        Tier::Rejected => Err(format!(
            "Command unconditionally rejected (catastrophic pattern detected): {}",
            command
        )),
    }
}
