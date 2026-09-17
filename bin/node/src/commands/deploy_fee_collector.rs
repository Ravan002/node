use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use miden_node_block_producer::{DEFAULT_VALIDATOR_TIMEOUT, deploy_fee_collector};
use miden_node_store::State;
use miden_node_tracing::info;
use miden_node_utils::clap::duration_to_human_readable_string;
use miden_node_utils::shutdown::CancellationToken;
use miden_protocol::account::auth::AuthSecretKey;
use miden_protocol::account::{AccountBuilder, AccountFile, AccountType};
use miden_protocol::utils::serde::Serializable;
use miden_standards::account::auth::AuthTxFeeCollector;
use miden_standards::account::wallets::BasicWallet;
use url::Url;

use super::ENV_DATA_DIRECTORY;
use super::store::StoreOptions;

#[cfg(test)]
mod tests;

#[derive(clap::Args, Debug)]
pub struct DeployFeeCollectorCommand {
    /// Directory containing the node's synced chain state. The sequencer must be stopped.
    #[arg(long, env = ENV_DATA_DIRECTORY, value_name = "DIR")]
    data_directory: PathBuf,

    /// Output file for the new collector account and signing key.
    #[arg(long, value_name = "FILE")]
    output: PathBuf,

    /// Overwrite the output file if it exists. This discards the previous account and signing key.
    #[arg(long)]
    clobber: bool,

    /// Validator service URLs. Repeat this option for each validator.
    #[arg(
        long = "validator.url",
        env = "MIDEN_NODE_VALIDATOR_URL",
        value_name = "URL",
        value_delimiter = ',',
        required = true
    )]
    validator_urls: Vec<Url>,

    /// Request timeout for calls to the validator services.
    #[arg(
        long = "validator.timeout",
        env = "MIDEN_NODE_VALIDATOR_TIMEOUT",
        default_value = duration_to_human_readable_string(DEFAULT_VALIDATOR_TIMEOUT),
        value_parser = humantime::parse_duration,
        value_name = "DURATION"
    )]
    validator_timeout: Duration,

    #[command(flatten)]
    store: StoreOptions,
}

impl DeployFeeCollectorCommand {
    pub async fn handle(self, shutdown: CancellationToken) -> anyhow::Result<()> {
        let loaded = State::load_with_database_options(
            &self.data_directory,
            self.store.storage.into(),
            self.store.sqlite.database_options(),
        )
        .await
        .context("failed to load node state")?;
        let (state, mut block_writer, mut proof_writer, writer_task) =
            loaded.start(CancellationToken::new());
        let result = async {
            anyhow::ensure!(
                state.proven_tip() == state.committed_tip(),
                "sync all committed block proofs before deploying a fee collector",
            );
            let account = create_collection_account(&self.output, self.clobber)?;
            info!(target: crate::LOG_TARGET, "Saved new fee collector account",
                account.id = account.account.id(),
                account.file = self.output.as_path());
            tokio::select! {
                () = shutdown.cancelled() => anyhow::bail!("fee collector deployment cancelled"),
                result = Box::pin(deploy_fee_collector(
                    &state,
                    &mut block_writer,
                    &mut proof_writer,
                    account,
                    self.validator_urls,
                    self.validator_timeout,
                )) => result,
            }
        }
        .await;
        block_writer.stop(writer_task).await;
        result
    }
}

/// Saves the signing key before deployment can change the chain.
fn create_collection_account(output: &Path, clobber: bool) -> anyhow::Result<AccountFile> {
    let secret_key = AuthSecretKey::new_falcon512_poseidon2();
    let account = AccountBuilder::new(rand::random())
        .account_type(AccountType::Public)
        .with_component(AuthTxFeeCollector::from_public_key(secret_key.public_key()))
        .with_component(BasicWallet)
        .build()?;
    let account_file = AccountFile::new(account, vec![secret_key]);
    let mut options = fs_err::OpenOptions::new();
    options.create_new(!clobber).create(clobber).truncate(clobber).write(true);
    #[cfg(unix)]
    {
        use fs_err::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(output).context("failed to create fee collector account file")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(&account_file.to_bytes())?;
    file.sync_all()?;
    Ok(account_file)
}
