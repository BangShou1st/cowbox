use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathBase {
    Home,
    Env(&'static str),
}

#[derive(Debug, Clone, Copy)]
pub struct PathSpec {
    pub base: PathBase,
    pub relative: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub struct Harness {
    pub id: &'static str,
    pub display: &'static str,
    pub paths: &'static [PathSpec],
}

const HOME: PathBase = PathBase::Home;
const HERMES: PathBase = PathBase::Env("HERMES_HOME");

pub const ALL_HARNESSES: &[Harness] = &[
    Harness {
        id: "codex",
        display: "Codex",
        paths: &[
            PathSpec {
                base: HOME,
                relative: ".agents/skills",
            },
            PathSpec {
                base: HOME,
                relative: ".codex/skills",
            },
        ],
    },
    Harness {
        id: "codex-internal",
        display: "Codex (internal)",
        paths: &[PathSpec {
            base: HOME,
            relative: ".codex-internal/skills",
        }],
    },
    Harness {
        id: "claude",
        display: "Claude Code",
        paths: &[PathSpec {
            base: HOME,
            relative: ".claude/skills",
        }],
    },
    Harness {
        id: "claude-internal",
        display: "Claude Code (internal)",
        paths: &[PathSpec {
            base: HOME,
            relative: ".claude-internal/skills",
        }],
    },
    Harness {
        id: "cursor",
        display: "Cursor",
        paths: &[PathSpec {
            base: HOME,
            relative: ".cursor/skills",
        }],
    },
    Harness {
        id: "openclaw",
        display: "OpenClaw",
        paths: &[PathSpec {
            base: HOME,
            relative: ".openclaw/skills",
        }],
    },
    Harness {
        id: "openclaw-internal",
        display: "OpenClaw (internal)",
        paths: &[PathSpec {
            base: HOME,
            relative: ".openclaw-internal/skills",
        }],
    },
    Harness {
        id: "codebuddy",
        display: "CodeBuddy",
        paths: &[PathSpec {
            base: HOME,
            relative: ".codebuddy/skills",
        }],
    },
    Harness {
        id: "workbuddy",
        display: "WorkBuddy",
        paths: &[PathSpec {
            base: HOME,
            relative: ".workbuddy/skills",
        }],
    },
    Harness {
        id: "qoder",
        display: "Qoder",
        paths: &[PathSpec {
            base: HOME,
            relative: ".qoder/skills",
        }],
    },
    Harness {
        id: "qoder-cn",
        display: "Qoder CN",
        paths: &[PathSpec {
            base: HOME,
            relative: ".qoder-cn/skills",
        }],
    },
    Harness {
        id: "pi",
        display: "Pi",
        paths: &[PathSpec {
            base: HOME,
            relative: ".pi/agent/skills",
        }],
    },
    Harness {
        id: "hermes",
        display: "Hermes Agent",
        paths: &[PathSpec {
            base: HERMES,
            relative: "skills",
        }],
    },
    Harness {
        id: "kimi",
        display: "Kimi Code",
        paths: &[PathSpec {
            base: HOME,
            relative: ".kimi-code/skills",
        }],
    },
];

pub fn find(id: &str) -> Option<&'static Harness> {
    ALL_HARNESSES.iter().find(|harness| harness.id == id)
}

pub fn resolve_paths(harness: &Harness, home: &std::path::Path) -> Vec<PathBuf> {
    resolve_paths_with(harness, home, |name| {
        std::env::var_os(name).map(PathBuf::from)
    })
}

pub fn resolve_paths_with<F>(harness: &Harness, home: &std::path::Path, env: F) -> Vec<PathBuf>
where
    F: Fn(&str) -> Option<PathBuf>,
{
    harness
        .paths
        .iter()
        .filter_map(|spec| match spec.base {
            PathBase::Home => Some(home.join(spec.relative)),
            PathBase::Env(name) => env(name).map(|root| root.join(spec.relative)),
        })
        .collect()
}

pub fn detected(paths: &[PathBuf]) -> bool {
    paths
        .iter()
        .any(|path| path.parent().map(|parent| parent.exists()).unwrap_or(false) || path.exists())
}
