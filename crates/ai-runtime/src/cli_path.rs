use std::ffi::OsStr;
use std::path::{Path, PathBuf};

pub(crate) fn installed_cli(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let data_home = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from);
    let pnpm_home = std::env::var_os("PNPM_HOME").map(PathBuf::from);
    find_cli(
        name,
        &path,
        home.as_deref(),
        data_home.as_deref(),
        pnpm_home.as_deref(),
    )
}

fn find_cli(
    name: &str,
    path: &OsStr,
    home: Option<&Path>,
    data_home: Option<&Path>,
    pnpm_home: Option<&Path>,
) -> Option<PathBuf> {
    // Desktop launchers often inherit only system PATH entries. Prefer the
    // user's installs, matching their signed-in terminal session.
    let user_bin = home.map(|home| home.join(".local/bin"));
    let pnpm = pnpm_home
        .filter(|path| path.is_absolute())
        .map(Path::to_path_buf)
        .or_else(|| {
            data_home
                .filter(|path| path.is_absolute())
                .map(Path::to_path_buf)
                .or_else(|| home.map(|home| home.join(".local/share")))
                .map(|data| data.join("pnpm"))
        });
    user_bin
        .into_iter()
        .chain(pnpm)
        .chain(std::env::split_paths(path))
        .filter(|directory| directory.is_absolute())
        .map(|directory| directory.join(name))
        .find(|path| {
            let Ok(metadata) = path.metadata() else {
                return false;
            };
            if !metadata.is_file() {
                return false;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                metadata.permissions().mode() & 0o111 != 0
            }
            #[cfg(not(unix))]
            {
                true
            }
        })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn executable(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"fixture must never execute").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[test]
    fn desktop_path_finds_user_installed_providers() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let system = root.path().join("system-bin");
        let path = std::env::join_paths([&system]).unwrap();
        for (name, relative) in [
            ("claude", ".local/bin/claude"),
            ("codex", ".local/share/pnpm/codex"),
        ] {
            let expected = home.join(relative);
            executable(&expected);
            assert_eq!(
                find_cli(name, &path, Some(&home), None, None),
                Some(expected)
            );
        }
    }

    #[test]
    fn user_install_wins_over_an_old_system_install() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let system = root.path().join("system-bin");
        let user = home.join(".local/share/pnpm/codex");
        executable(&user);
        executable(&system.join("codex"));
        let path = std::env::join_paths([&system]).unwrap();
        assert_eq!(
            find_cli("codex", &path, Some(&home), None, None),
            Some(user)
        );
    }

    #[test]
    fn configured_pnpm_and_xdg_locations_are_used_without_shell_setup() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        let pnpm = root.path().join("pnpm");
        let xdg = data.join("pnpm/codex");
        executable(&xdg);
        assert_eq!(
            find_cli("codex", OsStr::new(""), None, Some(&data), None),
            Some(xdg)
        );
        let configured = pnpm.join("codex");
        executable(&configured);
        assert_eq!(
            find_cli("codex", OsStr::new(""), None, Some(&data), Some(&pnpm)),
            Some(configured)
        );
    }

    #[test]
    fn empty_or_relative_user_paths_keep_the_home_default() {
        let root = tempfile::tempdir().unwrap();
        let expected = root.path().join(".local/share/pnpm/codex");
        executable(&expected);
        for invalid in [Path::new(""), Path::new("relative")] {
            assert_eq!(
                find_cli(
                    "codex",
                    OsStr::new(""),
                    Some(root.path()),
                    Some(invalid),
                    Some(invalid)
                ),
                Some(expected.clone())
            );
        }
    }

    #[test]
    fn missing_or_nonexecutable_user_install_falls_back_to_absolute_path() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let system = root.path().join("system-bin");
        let user = home.join(".local/bin/claude");
        executable(&user);
        std::fs::set_permissions(&user, std::fs::Permissions::from_mode(0o600)).unwrap();
        let fallback = system.join("claude");
        executable(&fallback);
        let path = std::env::join_paths([Path::new("."), &system]).unwrap();
        assert_eq!(
            find_cli("claude", &path, Some(&home), None, None),
            Some(fallback)
        );
        assert_eq!(find_cli("claude", OsStr::new("."), None, None, None), None);
    }
}
