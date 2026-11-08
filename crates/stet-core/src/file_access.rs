// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Which files on the host a PostScript program may read, write, delete
//! and rename.
//!
//! Under [`FileAccess::Full`], the default, PostScript's file operators
//! reach the whole file system, as in Ghostscript with `-dNOSAFER`. Under
//! [`FileAccess::Safer`] they reach only:
//!
//! * for reading (`file` with `(r)`, `run`): stet's on-disk resource
//!   folder (`Context::resource_base_path`), the installed fonts stet has
//!   found, and the paths permitted for reading;
//! * for writing (`file` with `(w)` or `(a)`): the paths permitted for
//!   writing. `(r+)` needs both;
//! * for `deletefile` and `renamefile`: the paths permitted for control.
//!   `renamefile` needs control on both names and write on the new one.
//!
//! `status` and `filenameforall` see a path under any permit and nothing
//! else. Embedded resources and `%stdin`, `%stdout` and `%stderr` are
//! never restricted. This is Ghostscript's `-dSAFER` with its
//! `--permit-file-read`, `--permit-file-write` and
//! `--permit-file-control`, and unlike
//! [`deny_host_access`](stet_fonts::host::deny_host_access) it leaves
//! stet's own font, CMap and colour-profile lookups alone, so pages render
//! as they do under `Full`. `deny_host_access` still wins over every
//! permit.
//!
//! A path is judged where it really is: symbolic links and `..` are
//! resolved as the operating system resolves them, so neither leads out of
//! a permitted folder. A path outside every permit is refused before
//! anything about it is read, so the refusal is the same whether or not
//! the file exists.

use std::io;
use std::path::{Path, PathBuf};

/// How much of the host's file system PostScript's file operators reach.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FileAccess {
    /// Every file the process can reach. The default.
    #[default]
    Full,
    /// Only stet's own resources, installed fonts, and the paths in
    /// [`FilePermits`]. See the [module documentation](self).
    Safer,
}

/// What a program may do with a file, for [`FilePermits`].
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Open it for reading, or `run` it.
    Read,
    /// Open it for writing or appending.
    Write,
    /// Delete it, or rename it.
    Control,
}

/// One permitted path.
#[derive(Debug, Clone)]
struct Permit {
    access: Access,
    /// Where it really is ([`real_path`]).
    path: PathBuf,
    /// A folder permits everything inside it; a file only itself.
    folder: bool,
}

/// The paths a program may read, write or control under
/// [`FileAccess::Safer`].
#[derive(Debug, Clone, Default)]
pub struct FilePermits {
    permits: Vec<Permit>,
}

impl FilePermits {
    /// No paths permitted.
    pub const fn new() -> Self {
        Self {
            permits: Vec::new(),
        }
    }

    /// Permit `access` to `path`: everything inside it if it is a folder,
    /// and only itself if it is a file. The file need not exist yet, so a
    /// job can be let write one output file, but the folder it would be in
    /// must: otherwise this is an error, since the path cannot be resolved
    /// and a mistyped one would permit nothing.
    pub fn add(&mut self, access: Access, path: impl AsRef<Path>) -> io::Result<()> {
        let path = path.as_ref();
        let folder = path.is_dir();
        let real = real_path(path).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("cannot permit {}: no such folder", path.display()),
            )
        })?;
        if !folder && !real.parent().is_some_and(Path::is_dir) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("cannot permit {}: no such folder", path.display()),
            ));
        }
        self.permits.push(Permit {
            access,
            path: real,
            folder,
        });
        Ok(())
    }

    /// True when nothing is permitted.
    pub fn is_empty(&self) -> bool {
        self.permits.is_empty()
    }

    /// Whether `real`, a path as [`real_path`] gives it, is permitted
    /// `access`.
    fn allows(&self, access: Access, real: &Path) -> bool {
        self.permits.iter().any(|p| {
            p.access == access
                && if p.folder {
                    real.starts_with(&p.path)
                } else {
                    real == p.path
                }
        })
    }
}

/// The rule [`FileStore`](crate::file_store::FileStore) and the file
/// operators apply before touching a file on the host: the
/// [`Context`](crate::context::Context)'s [`FileAccess`], its permits and
/// its resource folder. Built from the context's fields with
/// [`Context::host_policy`](crate::context::Context::host_policy), or from
/// the fields themselves where the context is borrowed elsewhere.
#[derive(Debug, Clone, Copy)]
pub struct HostPolicy<'a> {
    access: FileAccess,
    permits: &'a FilePermits,
    resource_base: Option<&'a str>,
}

/// The permits of [`HostPolicy::full`]: none, and unread under `Full`.
static NO_PERMITS: FilePermits = FilePermits::new();

impl<'a> HostPolicy<'a> {
    /// The rule for `access`, `permits` and the on-disk resource folder.
    pub fn new(
        access: FileAccess,
        permits: &'a FilePermits,
        resource_base: Option<&'a str>,
    ) -> Self {
        Self {
            access,
            permits,
            resource_base,
        }
    }

    /// [`FileAccess::Full`]: every path, unless
    /// [`deny_host_access`](stet_fonts::host::deny_host_access) was called.
    pub fn full() -> HostPolicy<'static> {
        HostPolicy {
            access: FileAccess::Full,
            permits: &NO_PERMITS,
            resource_base: None,
        }
    }

    /// Whether a program may have `access` to the host file `path`.
    pub fn allows(&self, path: &Path, access: Access) -> bool {
        if !stet_fonts::host::host_access_allowed() {
            return false;
        }
        match self.access {
            FileAccess::Full => true,
            FileAccess::Safer => self.safer_allows(path, access),
        }
    }

    /// Whether a program may see that the host file `path` exists, with
    /// `status` or `filenameforall`: when it has any access to it.
    pub fn sees(&self, path: &Path) -> bool {
        [Access::Read, Access::Write, Access::Control]
            .into_iter()
            .any(|access| self.allows(path, access))
    }

    fn safer_allows(&self, path: &Path, access: Access) -> bool {
        if let Some(real) = real_path(path) {
            if self.permits.allows(access, &real) {
                return true;
            }
            if access == Access::Read
                && let Some(base) = self.resource_base
                && let Ok(base) = Path::new(base).canonicalize()
                && real.starts_with(base)
            {
                return true;
            }
        }
        // Last, since the first call lists the machine's fonts: a font
        // stet found installed, which fontcategory.ps `run`s by the path
        // `.loadsystemfont` gave it. Compared as given, so the file opened
        // is the one listed; the list holds absolute paths only.
        access == Access::Read
            && path.is_absolute()
            && stet_fonts::system_fonts::get_system_font_cache()
                .iter()
                .any(|(_, font)| font == path)
    }
}

/// Where `path` really is, as the operating system would open it: the
/// canonical path where it exists, and otherwise its longest existing
/// beginning made canonical with the rest of the names appended. `None`
/// when the part that does not exist holds anything but plain names (a
/// `..` there cannot be resolved), or a relative path is given and the
/// working directory cannot be read.
fn real_path(path: &Path) -> Option<PathBuf> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    if let Ok(real) = path.canonicalize() {
        return Some(real);
    }
    let mut missing = Vec::new();
    let mut existing = path.as_path();
    loop {
        missing.push(existing.file_name()?);
        existing = existing.parent()?;
        if let Ok(mut real) = existing.canonicalize() {
            for name in missing.iter().rev() {
                real.push(name);
            }
            return Some(real);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch folder with `inside/` (holding `a.txt` and a link out),
    /// and `outside/` holding `secret.txt`, beside it.
    fn scratch(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("stet-access-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(root.join("inside")).unwrap();
        std::fs::create_dir_all(root.join("outside")).unwrap();
        std::fs::write(root.join("inside/a.txt"), b"a").unwrap();
        std::fs::write(root.join("outside/secret.txt"), b"s").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("outside"), root.join("inside/out")).unwrap();
        root.canonicalize().unwrap()
    }

    fn safer(permits: &FilePermits) -> HostPolicy<'_> {
        HostPolicy::new(FileAccess::Safer, permits, None)
    }

    #[test]
    fn a_permitted_folder_permits_what_is_inside_it_and_nothing_else() {
        let root = scratch("folder");
        let mut permits = FilePermits::new();
        permits.add(Access::Read, root.join("inside")).unwrap();
        let policy = safer(&permits);

        assert!(policy.allows(&root.join("inside/a.txt"), Access::Read));
        assert!(policy.allows(&root.join("inside/new.txt"), Access::Read));
        assert!(!policy.allows(&root.join("inside/a.txt"), Access::Write));
        assert!(!policy.allows(&root.join("outside/secret.txt"), Access::Read));
        assert!(!policy.allows(&root.join("inside/../outside/secret.txt"), Access::Read));
        assert!(!policy.allows(&root.join("inside/../outside/missing"), Access::Read));
        assert!(!policy.allows(&root.join("inside/no/../../outside/x"), Access::Read));
        #[cfg(unix)]
        {
            assert!(!policy.allows(&root.join("inside/out/secret.txt"), Access::Read));
            assert!(!policy.allows(&root.join("inside/out/missing"), Access::Read));
        }
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_permitted_file_permits_only_itself_and_need_not_exist() {
        let root = scratch("file");
        let mut permits = FilePermits::new();
        permits
            .add(Access::Write, root.join("inside/out.txt"))
            .unwrap();
        let policy = safer(&permits);

        assert!(policy.allows(&root.join("inside/out.txt"), Access::Write));
        assert!(!policy.allows(&root.join("inside/a.txt"), Access::Write));
        assert!(!policy.allows(&root.join("inside/out.txt"), Access::Read));

        let missing_folder = permits.add(Access::Write, root.join("nowhere/out.txt"));
        assert_eq!(missing_folder.unwrap_err().kind(), io::ErrorKind::NotFound);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_resource_folder_is_readable_and_full_access_allows_everything() {
        let root = scratch("resources");
        let permits = FilePermits::new();
        let base = root.join("inside").to_string_lossy().into_owned();
        let policy = HostPolicy::new(FileAccess::Safer, &permits, Some(&base));

        assert!(policy.allows(&root.join("inside/a.txt"), Access::Read));
        assert!(!policy.allows(&root.join("inside/a.txt"), Access::Write));
        assert!(!policy.allows(&root.join("outside/secret.txt"), Access::Read));
        assert!(policy.sees(&root.join("inside/a.txt")));
        assert!(!policy.sees(&root.join("outside/secret.txt")));

        let full = HostPolicy::full();
        assert!(full.allows(&root.join("outside/secret.txt"), Access::Control));
        std::fs::remove_dir_all(&root).ok();
    }
}
