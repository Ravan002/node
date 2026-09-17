use std::time::Duration;

use miden_node_store::State;
use miden_node_store::genesis::GenesisBlock;
use miden_node_utils::clap::StorageOptions;
use miden_node_utils::shutdown::CancellationToken;
use miden_protocol::block::{BlockSignatures, SignedBlock};
use miden_testing::MockChain;

use super::*;
use crate::batch_builder::BatchIntervals;
use crate::server::BlockProducerApiConfig;
use crate::test_utils::mock_collection_account;
use crate::{DEFAULT_BATCH_INTERVAL, DEFAULT_BATCH_WORKERS, DEFAULT_BLOCK_INTERVAL};

#[tokio::test(flavor = "multi_thread")]
#[expect(
    clippy::too_many_lines,
    reason = "Keep deployment and restart checks in one chain fixture"
)]
async fn deployment_waits_for_commitment_and_reuses_the_account_after_restart() -> anyhow::Result<()>
{
    let mut chain = MockChain::builder().verification_base_fee(1).build()?;
    let genesis = chain.latest_block();
    let directory = tempfile::tempdir()?;
    State::bootstrap(
        GenesisBlock::new(
            SignedBlock::new(
                genesis.header().clone(),
                genesis.body().clone(),
                BlockSignatures::new(vec![])?,
            )?,
            chain.protocol_config().clone(),
        )?,
        directory.path(),
    )?;
    let shutdown = CancellationToken::new();
    let (state, mut writer, _proof_writer, writer_task) =
        State::load(directory.path(), StorageOptions::default())
            .await?
            .start(shutdown.clone());
    let api = BlockProducerApi::new(
        Arc::clone(&state),
        state.committed_tip(),
        BlockProducerApiConfig::default(),
        shutdown.clone(),
    );
    let account = mock_collection_account();
    let target = miden_protocol::account::AccountId::from_hex("0xcc0000000000dd010000ee000000ff")?;
    let mut builder = BatchBuilder::new(
        Arc::clone(&state),
        DEFAULT_BATCH_WORKERS,
        None,
        BatchIntervals::derive_from(DEFAULT_BLOCK_INTERVAL, DEFAULT_BATCH_INTERVAL),
        target,
        account.clone(),
    )?;
    let executed = builder
        .pass_through
        .execute(
            Vec::new(),
            chain.latest_block_header(),
            chain.protocol_config().clone(),
            chain.latest_partial_blockchain(),
        )
        .await?;
    let deployment_api = api.clone();
    let mut deployment = tokio::spawn(async move {
        builder.deploy_collection_account(&deployment_api).await?;
        anyhow::Ok(builder)
    });
    tokio::time::timeout(Duration::from_secs(180), async {
        loop {
            tokio::select! {
                result = &mut deployment => {
                    result??;
                    anyhow::bail!("deployment completed before commitment");
                },
                () = tokio::time::sleep(Duration::from_millis(100)) => {
                    if api.status().await.mempool_stats.proven_batches == 1 {
                        break anyhow::Ok(());
                    }
                },
            }
        }
    })
    .await??;

    chain.add_pending_executed_transaction(&executed)?;
    let block = chain.prove_next_block()?;
    writer
        .apply_block(
            SignedBlock::new(
                block.header().clone(),
                block.body().clone(),
                block.signatures().clone(),
            )?,
            None,
        )
        .await?;
    let builder = tokio::time::timeout(Duration::from_secs(10), deployment).await???;
    assert!(!builder.pass_through.account.is_new());
    assert_eq!(
        builder.pass_through.account.to_commitment(),
        executed.final_account().to_commitment()
    );

    let restarted_api = BlockProducerApi::new(
        Arc::clone(&state),
        state.committed_tip(),
        BlockProducerApiConfig::default(),
        shutdown.clone(),
    );
    let mut restarted = BatchBuilder::new(
        state,
        DEFAULT_BATCH_WORKERS,
        None,
        BatchIntervals::derive_from(DEFAULT_BLOCK_INTERVAL, DEFAULT_BATCH_INTERVAL),
        target,
        account,
    )?;
    tokio::time::timeout(
        Duration::from_secs(10),
        restarted.deploy_collection_account(&restarted_api),
    )
    .await??;
    assert_eq!(
        restarted.pass_through.account.to_commitment(),
        builder.pass_through.account.to_commitment()
    );
    assert_eq!(restarted_api.status().await.mempool_stats.uncommitted_transactions, 0);

    shutdown.cancel();
    writer_task.await?;
    Ok(())
}
