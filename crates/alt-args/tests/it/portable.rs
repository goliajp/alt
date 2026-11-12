//! The part of the copied command line that splices one set of options
//! into many subcommands.
#![allow(dead_code)]

use alt_args::{Args, Subcommand};

#[derive(Args)]
pub struct PortableOptions {
    /// Private credential created by alt auth login
    #[arg(long)]
    pub credential_file: std::path::PathBuf,
    /// Private durable journal; reuse this directory after an interrupted attempt
    #[arg(long)]
    pub journal: std::path::PathBuf,
    #[arg(long, default_value_t = 16_777_216)]
    pub max_metadata_bytes: usize,
    #[arg(long, default_value_t = 1_000_000)]
    pub max_entries: usize,
    #[arg(long, default_value_t = 100_000)]
    pub max_publications: usize,
    #[arg(long, default_value_t = 8_589_934_592)]
    pub max_blob_bytes: u64,
    #[arg(long, default_value_t = 68_719_476_736)]
    pub max_total_bytes: u64,
    #[arg(long, default_value_t = 120)]
    pub timeout_seconds: u64,
}

#[derive(Subcommand)]
pub enum PortableOp {
    /// Download a complete signed snapshot without stopping source writes
    Export {
        #[command(flatten)]
        options: PortableOptions,
        #[arg(long)]
        repository: String,
        #[arg(long)]
        output: std::path::PathBuf,
    },
    /// Inspect the same immutable source export
    ExportStatus {
        #[command(flatten)]
        options: PortableOptions,
    },
    /// Stop source writes only at the original export watermark
    FreezeSource {
        #[command(flatten)]
        options: PortableOptions,
    },
    /// Explicitly remove the original source write barrier
    ReopenSource {
        #[command(flatten)]
        options: PortableOptions,
    },
    /// Release retention for the original export; a frozen source must be reopened first
    RetireExport {
        #[command(flatten)]
        options: PortableOptions,
    },
    /// Verify a local archive and stage it privately in a new empty target repository
    Import {
        #[command(flatten)]
        options: PortableOptions,
        #[arg(long)]
        repository: String,
        #[arg(long)]
        archive: std::path::PathBuf,
        /// JSON list of explicit source identity dispositions; null links preserve history only
        #[arg(long)]
        mappings: std::path::PathBuf,
        #[arg(long)]
        source_installation: String,
        #[arg(long)]
        source_repository: String,
        #[arg(long)]
        source_epoch: u64,
        /// Source receipt public key obtained independently of the archive
        #[arg(long)]
        source_key: String,
    },
    /// Query the original target intent, including a lost activation response
    ImportStatus {
        #[command(flatten)]
        options: PortableOptions,
    },
    /// Atomically activate the original staged import under current target authority
    ActivateImport {
        #[command(flatten)]
        options: PortableOptions,
    },
    /// Abandon a receiving import without deleting the source or accepted history
    AbandonImport {
        #[command(flatten)]
        options: PortableOptions,
    },
}
