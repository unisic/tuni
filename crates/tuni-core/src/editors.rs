//! Which code editors are installed, for the Info page's "Open in Editor"
//! menu.
//!
//! Detection is a scan of `PATH` for the command each editor puts down,
//! because the command is the one thing every install method — a distribution
//! package, a tarball, the JetBrains Toolbox — agrees on, and the command is
//! also how the editor is opened on a directory. Nothing is linked and no
//! desktop database is read: an editor that did not put its command on `PATH`
//! has said it does not want to be driven from outside.

use std::path::Path;

/// An editor the menu can offer: the name a person knows it by, the command
/// that opens it on a directory, and the desktop-file ids its icon may be
/// filed under — several, because the id depends on who installed it: the
/// distribution package, the vendor's own, or the JetBrains Toolbox each
/// write a different one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Editor {
    pub name: &'static str,
    pub command: &'static str,
    pub desktop: &'static [&'static str],
}

/// The editors worth looking for. Terminal editors are not here on purpose:
/// tuni is the terminal, and `vim .` typed at the prompt already works.
const EDITORS: &[Editor] = &[
    Editor {
        name: "Zed",
        command: "zed",
        desktop: &["dev.zed.Zed.desktop", "zed.desktop"],
    },
    Editor {
        name: "VS Code",
        command: "code",
        desktop: &[
            "code.desktop",
            "visual-studio-code.desktop",
            "com.visualstudio.code.desktop",
        ],
    },
    Editor {
        name: "VSCodium",
        command: "codium",
        desktop: &["codium.desktop", "vscodium.desktop"],
    },
    Editor {
        name: "Sublime Text",
        command: "subl",
        desktop: &["sublime_text.desktop", "sublime-text.desktop"],
    },
    Editor {
        name: "Kate",
        command: "kate",
        desktop: &["org.kde.kate.desktop"],
    },
    Editor {
        name: "GNOME Builder",
        command: "gnome-builder",
        desktop: &["org.gnome.Builder.desktop"],
    },
    Editor {
        name: "IntelliJ IDEA",
        command: "idea",
        desktop: &[
            "jetbrains-idea.desktop",
            "jetbrains-idea-ce.desktop",
            "intellij-idea-ultimate-edition.desktop",
        ],
    },
    Editor {
        name: "PyCharm",
        command: "pycharm",
        desktop: &[
            "jetbrains-pycharm.desktop",
            "jetbrains-pycharm-ce.desktop",
            "pycharm-professional.desktop",
        ],
    },
    Editor {
        name: "WebStorm",
        command: "webstorm",
        desktop: &["jetbrains-webstorm.desktop"],
    },
    Editor {
        name: "CLion",
        command: "clion",
        desktop: &["jetbrains-clion.desktop"],
    },
    Editor {
        name: "GoLand",
        command: "goland",
        desktop: &["jetbrains-goland.desktop"],
    },
    Editor {
        name: "RustRover",
        command: "rustrover",
        desktop: &["jetbrains-rustrover.desktop"],
    },
];

/// Every known editor whose command is on `PATH` right now, in the table's
/// order. Looked up when the menu is built rather than cached: a handful of
/// `stat` calls is cheaper than being wrong after someone installs one.
#[must_use]
pub fn installed() -> Vec<Editor> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let directories: Vec<_> = std::env::split_paths(&path).collect();
    EDITORS
        .iter()
        .copied()
        .filter(|editor| {
            directories
                .iter()
                .any(|directory| runs(&directory.join(editor.command)))
        })
        .collect()
}

/// Whether a path is something that would run: a file with an execute bit.
/// The distinction matters because `PATH` lookups make it: a readable file
/// named `code` that cannot be executed is not Visual Studio Code.
fn runs(candidate: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    candidate
        .metadata()
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn an_editor_is_its_command_with_an_execute_bit() {
        let directory = std::env::temp_dir().join(format!("tuni-editors-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let command = directory.join("zed");
        std::fs::write(&command, "#!/bin/sh\n").unwrap();

        std::fs::set_permissions(&command, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(runs(&command));

        std::fs::set_permissions(&command, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(!runs(&command));

        assert!(!runs(&directory));
        std::fs::remove_dir_all(&directory).ok();
    }
}
