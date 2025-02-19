use crate::{satisfies::Satisfies, Base};

use std::collections::{HashMap, HashSet};

use alpm::{Alpm, Dep, DepMod, Depend};
use raur::ArcPackage;
use srcinfo::Srcinfo;

type ConflictMap = HashMap<String, Conflict>;

/// The response from resolving dependencies.
///
/// Note that just because resolving returned Ok() does not mean it is safe to bindly start
/// installing these packages.
#[derive(Debug)]
pub struct Actions<'a> {
    pub(crate) alpm: &'a Alpm,
    /// Some of the targets or dependencies could not be satisfied. This should be treated as
    /// a hard error.
    pub missing: Vec<Missing>,
    /// Targets that are up to date.
    pub unneeded: Vec<Unneeded>,
    /// Aur packages to build.
    pub build: Vec<Base>,
    /// Repo packages to install.
    pub install: Vec<RepoPackage<'a>>,
}

impl<'a> Actions<'a> {
    /// An iterator over each AUR package in self.build.
    pub fn iter_aur_pkgs(&self) -> impl Iterator<Item = &AurPackage> {
        self.build
            .iter()
            .filter_map(|b| match b {
                Base::Aur(pkg) => Some(&pkg.pkgs),
                Base::Custom(_) => None,
            })
            .flatten()
    }

    /// An iterator over each custom package in self.build.
    pub fn iter_custom_pkgs(&self) -> impl Iterator<Item = (&srcinfo::Srcinfo, &CustomPackage)> {
        self.build
            .iter()
            .filter_map(|b| match b {
                Base::Aur(_) => None,
                Base::Custom(base) => Some((&base.srcinfo, &base.pkgs)),
            })
            .flat_map(|(base, pkgs)| pkgs.iter().map(move |p| (base.as_ref(), p)))
    }
}

/// Information about an up to date package
#[derive(Debug, Eq, Clone, PartialEq, Ord, PartialOrd, Hash)]
pub struct Unneeded {
    /// Package name
    pub name: String,
    /// Package version
    pub version: String,
}

impl Unneeded {
    /// Create a new Unneeded
    pub fn new<S: Into<String>>(name: S, version: S) -> Self {
        Unneeded {
            name: name.into(),
            version: version.into(),
        }
    }
}

/// Wrapper around a package for extra metadata.
#[derive(Debug, Eq, Clone, PartialEq, Ord, PartialOrd, Hash)]
pub struct Package<T> {
    /// The underlying package
    pub pkg: T,
    /// If the package is only needed to build the targets.
    pub make: bool,
    /// If the package is a target.
    pub target: bool,
}

/// Wrapper around ArcPackage for extra metadata.
pub type AurPackage = Package<ArcPackage>;

/// Wrapper around Srcinfo for extra metadata.
pub type CustomPackage = Package<srcinfo::Package>;

/// Wrapper around alpm::Package for extra metadata.
pub type RepoPackage<'a> = Package<alpm::Package<'a>>;

/// A conflict
#[derive(Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Clone)]
pub struct Conflict {
    /// The name of the package.
    pub pkg: String,
    /// The packages conflicting with it.
    pub conflicting: Vec<Conflicting>,
}

/// A package that has conflicted with something
#[derive(Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Clone)]
pub struct Conflicting {
    /// The name of the package.
    pub pkg: String,
    /// The conflict that cause the confliction if it is different from the pkgname.
    pub conflict: Option<String>,
}

impl Conflict {
    /// Crate a new conflict.
    pub fn new(pkg: String) -> Self {
        Conflict {
            pkg,
            conflicting: Vec::with_capacity(1),
        }
    }

    /// Push a new conflicting to the conflict.
    pub fn push(&mut self, pkg: String, conflict: &Dep) {
        let conflict = if pkg != conflict.name() || conflict.depmod() != DepMod::Any {
            Some(conflict.to_string())
        } else {
            None
        };

        self.conflicting.push(Conflicting { pkg, conflict });
    }
}

/// An AUR package that should be updated.
#[derive(Debug)]
pub struct AurUpdate<'a> {
    /// The local package.
    pub local: alpm::Package<'a>,
    /// The AUR package.
    pub remote: ArcPackage,
}

/// An custom package that should be updated.
#[derive(Debug)]
pub struct CustomUpdate<'a> {
    /// The local package.
    pub local: alpm::Package<'a>,
    /// The custom repo the package belongs to.
    pub repo: String,
    /// The custom package base srcinfo.
    pub remote_srcinfo: &'a Srcinfo,
    /// The custom package base package,
    pub remote_pkg: &'a srcinfo::Package,
}

/// Collection of AUR updates and missing packages
#[derive(Debug, Default)]
pub struct AurUpdates<'a> {
    /// The updates.
    pub updates: Vec<AurUpdate<'a>>,
    /// Foreign that were not found in the AUR.
    pub missing: Vec<alpm::Package<'a>>,
    /// Packages that matched ignore pkg/group
    pub ignored: Vec<AurUpdate<'a>>,
}

/// Collection of custom updates and missing packages
#[derive(Debug, Default)]
pub struct CustomUpdates<'a> {
    /// The updates.
    pub updates: Vec<CustomUpdate<'a>>,
    /// Packages that matched ignore pkg/group
    pub ignored: Vec<CustomUpdate<'a>>,
}

/// Describes a package in the package stack.
#[derive(Debug, Clone, Default)]
pub struct DepMissing {
    /// The name of the package
    pub pkg: String,
    /// The dep string that pulled in the package. If it was different
    /// from the package name.
    pub dep: Option<String>,
}

/// A package that could not be resolved.
#[derive(Debug, Clone, Default)]
pub struct Missing {
    /// The Dependency we failed to satisfy.
    pub dep: String,
    /// The dependency path leadsing to pkg.
    pub stack: Vec<DepMissing>,
}

impl<'a> Actions<'a> {
    fn has_pkg<S: AsRef<str>>(&self, name: S) -> bool {
        let name = name.as_ref();
        let install = &self.install;
        self.iter_aur_pkgs().any(|pkg| pkg.pkg.name == name)
            || self.iter_custom_pkgs().any(|pkg| pkg.1.pkg.pkgname == name)
            || install.iter().any(|pkg| pkg.pkg.name() == name)
    }

    // check a conflict from locally installed pkgs, against install+build
    fn check_reverse_conflict<S: AsRef<str>>(
        &self,
        name: S,
        runtime: bool,
        conflict: &Dep,
        conflicts: &mut ConflictMap,
    ) {
        let name = name.as_ref();

        self.install
            .iter()
            .filter(|pkg| !runtime || !pkg.make)
            .map(|pkg| &pkg.pkg)
            .filter(|pkg| pkg.name() != name)
            .filter(|pkg| pkg.satisfies_dep(conflict, false))
            .for_each(|pkg| {
                conflicts
                    .entry(pkg.name().to_string())
                    .or_insert_with(|| Conflict::new(pkg.name().to_string()))
                    .push(name.to_string(), conflict);
            });

        self.iter_aur_pkgs()
            .filter(|pkg| !runtime || !pkg.make)
            .map(|pkg| &pkg.pkg)
            .filter(|pkg| pkg.name != name)
            .filter(|pkg| pkg.satisfies_dep(conflict, false))
            .for_each(|pkg| {
                conflicts
                    .entry(pkg.name.to_string())
                    .or_insert_with(|| Conflict::new(pkg.name.to_string()))
                    .push(name.to_string(), conflict);
            });
        self.iter_custom_pkgs()
            .filter(|(_, pkg)| !runtime || !pkg.make)
            .filter(|(_, pkg)| pkg.pkg.pkgname != name)
            .filter(|(base, pkg)| (*base, &pkg.pkg).satisfies_dep(conflict, false))
            .map(|pkg| &pkg.1.pkg)
            .for_each(|pkg| {
                conflicts
                    .entry(pkg.pkgname.clone())
                    .or_insert_with(|| Conflict::new(pkg.pkgname.to_string()))
                    .push(name.to_string(), conflict);
            });
    }

    // check a conflict from install+build against all locally installed pkgs
    fn check_forward_conflict<S: AsRef<str>>(
        &self,
        name: S,
        conflict: &Dep,
        conflicts: &mut ConflictMap,
    ) {
        let name = name.as_ref();
        self.alpm
            .localdb()
            .pkgs()
            .iter()
            .filter(|pkg| !self.has_pkg(pkg.name()))
            .filter(|pkg| pkg.name() != name)
            .filter(|pkg| pkg.satisfies_dep(conflict, false))
            .for_each(|pkg| {
                conflicts
                    .entry(name.to_string())
                    .or_insert_with(|| Conflict::new(name.to_string()))
                    .push(pkg.name().to_string(), conflict);
            });
    }

    fn check_forward_conflicts(&self, runtime: bool, conflicts: &mut ConflictMap) {
        for pkg in self.install.iter() {
            if runtime && pkg.make {
                continue;
            }

            for conflict in pkg.pkg.conflicts() {
                self.check_forward_conflict(pkg.pkg.name(), &conflict, conflicts);
            }
        }

        for pkg in self.iter_aur_pkgs() {
            if runtime && pkg.make {
                continue;
            }

            for conflict in &pkg.pkg.conflicts {
                self.check_forward_conflict(
                    &pkg.pkg.name,
                    &Depend::new(conflict.to_string()),
                    conflicts,
                );
            }
        }
        for (_, pkg) in self.iter_custom_pkgs() {
            if runtime && pkg.make {
                continue;
            }

            for conflict in pkg
                .pkg
                .conflicts
                .iter()
                .filter(|c| {
                    c.arch.is_none() || c.arch.as_deref() == self.alpm.architectures().first()
                })
                .flat_map(|c| &c.vec)
            {
                self.check_forward_conflict(
                    &pkg.pkg.pkgname,
                    &Depend::new(conflict.clone()),
                    conflicts,
                );
            }
        }
    }

    fn check_inner_conflicts(&self, runtime: bool, conflicts: &mut ConflictMap) {
        for pkg in self.install.iter() {
            if runtime && pkg.make {
                continue;
            }

            for conflict in pkg.pkg.conflicts() {
                self.check_reverse_conflict(pkg.pkg.name(), runtime, &conflict, conflicts)
            }
        }

        for pkg in self.iter_aur_pkgs() {
            if runtime && pkg.make {
                continue;
            }

            for conflict in pkg.pkg.conflicts.iter() {
                self.check_reverse_conflict(
                    &pkg.pkg.name,
                    runtime,
                    &Depend::new(conflict.to_string()),
                    conflicts,
                )
            }
        }

        for (_, pkg) in self.iter_custom_pkgs() {
            if runtime && pkg.make {
                continue;
            }

            for conflict in pkg
                .pkg
                .conflicts
                .iter()
                .filter(|c| {
                    c.arch.is_none() || c.arch.as_deref() == self.alpm.architectures().first()
                })
                .flat_map(|c| &c.vec)
            {
                self.check_reverse_conflict(
                    &pkg.pkg.pkgname,
                    runtime,
                    &Depend::new(conflict.to_string()),
                    conflicts,
                )
            }
        }
    }

    fn check_reverse_conflicts(&self, runtime: bool, conflicts: &mut ConflictMap) {
        self.alpm
            .localdb()
            .pkgs()
            .iter()
            .filter(|pkg| !self.has_pkg(pkg.name()))
            .for_each(|pkg| {
                pkg.conflicts().iter().for_each(|conflict| {
                    self.check_reverse_conflict(pkg.name(), runtime, &conflict, conflicts)
                })
            });
    }

    /// Calculate conflicts. Do note that even with conflicts it can still be possible to continue and
    /// install the packages. Although that is not checked here.
    ///
    /// For example installing pacman-git will conflict with pacman. But the install will still
    /// succeed as long as the user hits yes to pacman's prompt to remove pacman.
    ///
    /// However other cases are more complex and can not be automatically resolved. So it is up to
    /// the user to decide how to handle these.
    ///
    /// makedeps: if true, include make dependencies in the conflict calculation.
    pub fn calculate_conflicts(&self, makedeps: bool) -> Vec<Conflict> {
        let mut conflicts = ConflictMap::new();

        self.check_reverse_conflicts(!makedeps, &mut conflicts);
        self.check_forward_conflicts(!makedeps, &mut conflicts);

        let mut conflicts = conflicts.into_values().collect::<Vec<Conflict>>();

        conflicts.sort();
        conflicts
    }

    /// Calculate inner conflicts. Do note that even with conflicts it can still be possible to continue and
    /// install the packages. Although that is not checked here.
    ///
    /// For example installing pacman-git will conflict with pacman. But the install will still
    /// succeed as long as the user hits yes to pacman's prompt to remove pacman.
    ///
    /// However other cases are more complex and can not be automatically resolved. So it is up to
    /// the user to decide how to handle these.
    ///
    /// makedeps: if true, include make dependencies in the conflict calculation.
    pub fn calculate_inner_conflicts(&self, makedeps: bool) -> Vec<Conflict> {
        let mut inner_conflicts = ConflictMap::new();

        self.check_inner_conflicts(!makedeps, &mut inner_conflicts);

        let mut inner_conflicts = inner_conflicts.into_values().collect::<Vec<Conflict>>();

        inner_conflicts.sort();
        inner_conflicts
    }

    /// Find duplicate targets. It is possible to have duplicate targets if packages with the same
    /// name exist across repos.
    pub fn duplicate_targets(&self) -> Vec<String> {
        let mut names = HashSet::new();

        let build = self.iter_aur_pkgs().map(|pkg| pkg.pkg.name.as_str());
        let custom = self
            .iter_custom_pkgs()
            .map(|pkg| pkg.1.pkg.pkgname.as_str());

        let duplicates = self
            .install
            .iter()
            .map(|pkg| pkg.pkg.name())
            .chain(build)
            .chain(custom)
            .filter(|&name| !names.insert(name))
            .map(Into::into)
            .collect::<Vec<_>>();

        duplicates
    }
}
