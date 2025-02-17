//! Loads "sysroot" crate.
//!
//! One confusing point here is that normally sysroot is a bunch of `.rlib`s,
//! but we can't process `.rlib` and need source code instead. The source code
//! is typically installed with `rustup component add rust-src` command.

use std::{env, fs, iter, ops, path::PathBuf, process::Command};

use anyhow::{format_err, Result};
use la_arena::{Arena, Idx};
use paths::{AbsPath, AbsPathBuf};

use crate::{utf8_stdout, ManifestPath};

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Sysroot {
    root: AbsPathBuf,
    crates: Arena<SysrootCrateData>,
}

pub(crate) type SysrootCrate = Idx<SysrootCrateData>;

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SysrootCrateData {
    pub name: String,
    pub root: ManifestPath,
    pub deps: Vec<SysrootCrate>,
}

impl ops::Index<SysrootCrate> for Sysroot {
    type Output = SysrootCrateData;
    fn index(&self, index: SysrootCrate) -> &SysrootCrateData {
        &self.crates[index]
    }
}

impl Sysroot {
    pub fn root(&self) -> &AbsPath {
        &self.root
    }

    pub fn public_deps(&self) -> impl Iterator<Item = (&'static str, SysrootCrate, bool)> + '_ {
        // core is added as a dependency before std in order to
        // mimic rustcs dependency order
        ["core", "alloc", "std"]
            .iter()
            .copied()
            .zip(iter::repeat(true))
            .chain(iter::once(("test", false)))
            .filter_map(move |(name, prelude)| Some((name, self.by_name(name)?, prelude)))
    }

    pub fn proc_macro(&self) -> Option<SysrootCrate> {
        self.by_name("proc_macro")
    }

    pub fn crates<'a>(&'a self) -> impl Iterator<Item = SysrootCrate> + ExactSizeIterator + 'a {
        self.crates.iter().map(|(id, _data)| id)
    }

    pub fn discover(dir: &AbsPath) -> Result<Sysroot> {
        tracing::debug!("Discovering sysroot for {}", dir.display());
        let sysroot_dir = discover_sysroot_dir(dir)?;
        let sysroot_src_dir = discover_sysroot_src_dir(&sysroot_dir, dir)?;
        let res = Sysroot::load(sysroot_src_dir)?;
        Ok(res)
    }

    pub fn discover_rustc(cargo_toml: &ManifestPath) -> Option<ManifestPath> {
        tracing::debug!("Discovering rustc source for {}", cargo_toml.display());
        let current_dir = cargo_toml.parent();
        discover_sysroot_dir(current_dir).ok().and_then(|sysroot_dir| get_rustc_src(&sysroot_dir))
    }

    pub fn load(sysroot_src_dir: AbsPathBuf) -> Result<Sysroot> {
        let mut sysroot = Sysroot { root: sysroot_src_dir, crates: Arena::default() };

        for path in SYSROOT_CRATES.trim().lines() {
            let name = path.split('/').last().unwrap();
            let root = [format!("{}/src/lib.rs", path), format!("lib{}/lib.rs", path)]
                .iter()
                .map(|it| sysroot.root.join(it))
                .filter_map(|it| ManifestPath::try_from(it).ok())
                .find(|it| fs::metadata(it).is_ok());

            if let Some(root) = root {
                sysroot.crates.alloc(SysrootCrateData {
                    name: name.into(),
                    root,
                    deps: Vec::new(),
                });
            }
        }

        if let Some(std) = sysroot.by_name("std") {
            for dep in STD_DEPS.trim().lines() {
                if let Some(dep) = sysroot.by_name(dep) {
                    sysroot.crates[std].deps.push(dep)
                }
            }
        }

        if let Some(alloc) = sysroot.by_name("alloc") {
            if let Some(core) = sysroot.by_name("core") {
                sysroot.crates[alloc].deps.push(core);
            }
        }

        if let Some(proc_macro) = sysroot.by_name("proc_macro") {
            if let Some(std) = sysroot.by_name("std") {
                sysroot.crates[proc_macro].deps.push(std);
            }
        }

        if sysroot.by_name("core").is_none() {
            let var_note = if env::var_os("RUST_SRC_PATH").is_some() {
                " (`RUST_SRC_PATH` might be incorrect, try unsetting it)"
            } else {
                ""
            };
            anyhow::bail!(
                "could not find libcore in sysroot path `{}`{}",
                sysroot.root.as_path().display(),
                var_note,
            );
        }

        Ok(sysroot)
    }

    fn by_name(&self, name: &str) -> Option<SysrootCrate> {
        let (id, _data) = self.crates.iter().find(|(_id, data)| data.name == name)?;
        Some(id)
    }
}

fn discover_sysroot_dir(current_dir: &AbsPath) -> Result<AbsPathBuf> {
    let mut rustc = Command::new(toolchain::rustc());
    rustc.current_dir(current_dir).args(&["--print", "sysroot"]);
    tracing::debug!("Discovering sysroot by {:?}", rustc);
    let stdout = utf8_stdout(rustc)?;
    Ok(AbsPathBuf::assert(PathBuf::from(stdout)))
}

fn discover_sysroot_src_dir(
    sysroot_path: &AbsPathBuf,
    current_dir: &AbsPath,
) -> Result<AbsPathBuf> {
    if let Ok(path) = env::var("RUST_SRC_PATH") {
        let path = AbsPathBuf::try_from(path.as_str())
            .map_err(|path| format_err!("RUST_SRC_PATH must be absolute: {}", path.display()))?;
        let core = path.join("core");
        if fs::metadata(&core).is_ok() {
            tracing::debug!("Discovered sysroot by RUST_SRC_PATH: {}", path.display());
            return Ok(path);
        }
        tracing::debug!("RUST_SRC_PATH is set, but is invalid (no core: {:?}), ignoring", core);
    }

    get_rust_src(sysroot_path)
        .or_else(|| {
            let mut rustup = Command::new(toolchain::rustup());
            rustup.current_dir(current_dir).args(&["component", "add", "rust-src"]);
            utf8_stdout(rustup).ok()?;
            get_rust_src(sysroot_path)
        })
        .ok_or_else(|| {
            format_err!(
                "\
can't load standard library from sysroot
{}
(discovered via `rustc --print sysroot`)
try installing the Rust source the same way you installed rustc",
                sysroot_path.display(),
            )
        })
}

fn get_rustc_src(sysroot_path: &AbsPath) -> Option<ManifestPath> {
    let rustc_src = sysroot_path.join("lib/rustlib/rustc-src/rust/compiler/rustc/Cargo.toml");
    let rustc_src = ManifestPath::try_from(rustc_src).ok()?;
    tracing::debug!("Checking for rustc source code: {}", rustc_src.display());
    if fs::metadata(&rustc_src).is_ok() {
        Some(rustc_src)
    } else {
        None
    }
}

fn get_rust_src(sysroot_path: &AbsPath) -> Option<AbsPathBuf> {
    let rust_src = sysroot_path.join("lib/rustlib/src/rust/library");
    tracing::debug!("Checking sysroot: {}", rust_src.display());
    if fs::metadata(&rust_src).is_ok() {
        Some(rust_src)
    } else {
        None
    }
}

const SYSROOT_CRATES: &str = "
alloc
core
panic_abort
panic_unwind
proc_macro
profiler_builtins
std
stdarch/crates/std_detect
term
test
unwind";

const STD_DEPS: &str = "
alloc
core
panic_abort
panic_unwind
profiler_builtins
std_detect
term
test
unwind";
