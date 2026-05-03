//! Runtime backend selection.
//!
//! The current compositor runs as a nested winit compositor. Production DRM
//! support will live behind the same boundary so Wayland protocol state can be
//! shared while backend-specific session, input, output, and presentation code
//! stays isolated.

use anyhow::{bail, Result};

pub mod udev;
pub mod winit;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    Winit,
    Udev,
}

impl BackendKind {
    pub fn from_env_and_args() -> Result<Self> {
        Self::from_env_and_args_values(
            std::env::var("DE_COMPOSITOR_BACKEND").ok(),
            std::env::args(),
        )
    }

    fn from_env_and_args_values<I, S>(env_backend: Option<String>, args: I) -> Result<Self>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut backend = env_backend;

        let mut args = args.into_iter().map(Into::into);
        let _program_name = args.next();
        while let Some(arg) = args.next() {
            if let Some(value) = arg.strip_prefix("--backend=") {
                backend = Some(value.to_owned());
                continue;
            }
            if arg == "--backend" {
                let Some(value) = args.next() else {
                    bail!("--backend requires one of: winit, udev");
                };
                backend = Some(value);
            }
        }

        match backend.as_deref().unwrap_or("winit") {
            "winit" => Ok(Self::Winit),
            "udev" | "drm" => Ok(Self::Udev),
            other => bail!("unknown backend {other:?}; expected one of: winit, udev"),
        }
    }
}

pub fn run(kind: BackendKind) -> Result<()> {
    match kind {
        BackendKind::Winit => winit::run(),
        BackendKind::Udev => udev::run(),
    }
}

#[cfg(test)]
mod tests {
    use super::BackendKind;

    #[test]
    fn defaults_to_winit_without_env_or_cli_backend() {
        let kind = BackendKind::from_env_and_args_values(None, ["compositor-slint"]);

        assert_eq!(kind.unwrap(), BackendKind::Winit);
    }

    #[test]
    fn accepts_udev_from_environment() {
        let kind =
            BackendKind::from_env_and_args_values(Some("udev".to_owned()), ["compositor-slint"]);

        assert_eq!(kind.unwrap(), BackendKind::Udev);
    }

    #[test]
    fn cli_backend_overrides_environment() {
        let kind = BackendKind::from_env_and_args_values(
            Some("winit".to_owned()),
            ["compositor-slint", "--backend=drm"],
        );

        assert_eq!(kind.unwrap(), BackendKind::Udev);
    }

    #[test]
    fn rejects_missing_backend_value() {
        let err = BackendKind::from_env_and_args_values(None, ["compositor-slint", "--backend"])
            .unwrap_err()
            .to_string();

        assert!(err.contains("--backend requires"));
    }

    #[test]
    fn rejects_unknown_backend() {
        let err =
            BackendKind::from_env_and_args_values(None, ["compositor-slint", "--backend=wayland"])
                .unwrap_err()
                .to_string();

        assert!(err.contains("unknown backend"));
    }
}
