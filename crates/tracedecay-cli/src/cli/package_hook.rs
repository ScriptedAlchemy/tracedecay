use clap::Subcommand;

#[derive(Subcommand)]
pub enum PackageHookAction {
    /// Run Scoop package lifecycle integration.
    Scoop {
        #[command(subcommand)]
        action: ScoopPackageHookAction,
    },
}

#[derive(Subcommand)]
pub enum ScoopPackageHookAction {
    /// Snapshot and quiesce a managed service before Scoop replaces the app tree.
    Prepare {
        #[arg(long, value_parser = ["tracedecay", "tracedecay-beta"])]
        package_id: String,
        #[arg(long)]
        state_file: std::path::PathBuf,
    },
    /// Restore a snapshotted managed service after Scoop installs the new binary.
    Restore {
        #[arg(long, value_parser = ["tracedecay", "tracedecay-beta"])]
        package_id: String,
        #[arg(long)]
        state_file: std::path::PathBuf,
    },
}
