use strum::{Display, EnumString, EnumVariantNames, IntoStaticStr, VariantNames};

/// When should snapshot archives be used at startup?
#[derive(
    Debug, Default, Clone, Copy, PartialEq, Eq, Display, EnumString, EnumVariantNames, IntoStaticStr,
)]
#[strum(serialize_all = "kebab-case")]
pub enum UseSnapshotArchivesAtStartup {
    /// If snapshot archives are used, they will be extracted and overwrite any existing state
    /// already on disk.  This will incur the associated runtime costs for extracting.
    #[default]
    Always,
    /// If snapshot archive are not used, then the local snapshot state already on disk is
    /// used instead.  If there is no local state on disk, startup will fail.
    Never,
}

impl UseSnapshotArchivesAtStartup {
    pub const fn variants() -> &'static [&'static str] {
        Self::VARIANTS
    }
}

pub mod cli {
    use super::*;

    pub const fn name() -> &'static str {
        "use_snapshot_archives_at_startup"
    }
    pub const fn long_name() -> &'static str {
        "use-snapshot-archives-at-startup"
    }
    pub const fn help() -> &'static str {
        "When should snapshot archives be used at startup?"
    }
    pub const fn long_help() -> &'static str {
        "At startup, when should snapshot archives be extracted \
        versus using what is already on disk? \
        \nSpecifying \"always\" will always startup by extracting snapshot archives \
        and disregard any snapshot-related state already on disk. \
        Note that starting up from snapshot archives will incur the runtime costs \
        associated with extracting the archives and rebuilding the local state. \
        \nSpecifying \"never\" will never startup from snapshot archives \
        and will only use snapshot-related state already on disk. \
        If there is no state already on disk, startup will fail. \
        Note, this will use the latest state available, \
        which may be newer than the latest snapshot archive."
    }
    pub const fn possible_values() -> &'static [&'static str] {
        UseSnapshotArchivesAtStartup::VARIANTS
    }
    pub fn default_value() -> &'static str {
        UseSnapshotArchivesAtStartup::default().into()
    }
}
