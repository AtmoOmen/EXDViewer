//! What the install holds, by directory.
//!
//! The path list is global and the presence map is per version, so a name is only a file when both
//! agree. Everything that has to know what is on disk without asking for it by name reads this: the
//! asset tree, the icon subset, and the packs a model can be animated from.

use std::rc::Rc;

use anyhow::{Result, bail};
use pathlist::{PathList, Presence};

pub struct Listing {
    paths: PathList,
    presence: Presence,
}

impl Listing {
    pub fn decode(paths: &[u8], presence: &[u8]) -> Result<Self> {
        let paths = PathList::decode(paths)?;
        let presence = Presence::decode(presence)?;
        // The map is indexed by position in the list, so a pair from different builds would hide
        // and reveal the wrong files rather than fail.
        if paths.list_id() != presence.list_id() {
            bail!(
                "此版本的文件映射基于路径列表 {:016x} 构建，但当前列表为 {:016x}。",
                presence.list_id(),
                paths.list_id(),
            );
        }
        Ok(Self { paths, presence })
    }

    pub fn paths(&self) -> &PathList {
        &self.paths
    }

    pub fn presence(&self) -> &Presence {
        &self.presence
    }

    /// Every path this version ships in a directory or below it, which the sorted directory table
    /// makes a range rather than a sweep. A directory is listed without a trailing slash, so the
    /// one named by `prefix` itself has to be matched separately from the ones under it.
    pub fn under(&self, prefix: &str) -> Vec<String> {
        let dirs = self.paths.dirs();
        let stem = prefix.strip_suffix('/').unwrap_or(prefix);
        let below = format!("{stem}/");
        let from = dirs.partition_point(|listed| &**listed < stem);
        let mut found = Vec::new();
        for (dir, path) in dirs.iter().enumerate().skip(from) {
            if &**path != stem && !path.starts_with(&below) {
                break;
            }
            let (Ok(offset), Ok(names)) = (self.paths.name_offset(dir), self.paths.names(dir))
            else {
                continue;
            };
            found.extend(
                names
                    .into_iter()
                    .enumerate()
                    .filter(|(at, _)| self.presence.contains(offset + at))
                    .map(|(_, name)| format!("{path}/{name}")),
            );
        }
        found
    }
}

/// What [`Backend::listing`](crate::backend::Backend::listing) answers with, since the list is
/// asked for from wherever needs it first and only fetched once.
#[derive(Clone)]
pub enum Listed {
    Loading,
    Ready(Rc<Listing>),
    Failed(Rc<str>),
}
