use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use anyhow::Context;
use miden_node_proto::domain::account::AccountRequest;
use miden_node_tracing::spawn::spawn_blocking_in_current_span;
use miden_node_tracing::{info, miden_instrument, miden_span_record};
use miden_protocol::batch::ProposedBatch;
use miden_protocol::{MIN_PROOF_SECURITY_LEVEL, ONE, Word};

use super::BatchBuilder;
use super::pass_through::PassThroughTransactionBuilder;
use crate::server::BlockProducerApi;
use crate::{COMPONENT, LOG_TARGET};

#[cfg(test)]
mod tests;

impl BatchBuilder {
    /// Deploys the collector without input notes, output notes, or a transaction fee.
    ///
    /// The collector changes state only during deployment. Batch workers must wait for this
    /// transaction to commit before they can use the collector concurrently.
    #[miden_instrument(target = COMPONENT, name = "batch_builder.deploy_collector", err)]
    pub(super) async fn deploy_collection_account(
        &mut self,
        api: &BlockProducerApi,
    ) -> anyhow::Result<()> {
        let mut deployed_account = self.pass_through.account.clone();
        deployed_account.set_nonce(ONE)?;
        let expected_commitment = deployed_account.to_commitment();
        miden_span_record!(account.id = deployed_account.id());
        let mut committed_tip = self.state.subscribe_committed_tip();
        let mut expiration = None;

        loop {
            let response = self
                .state
                .view()
                .get_account(AccountRequest {
                    account_id: deployed_account.id(),
                    block_num: None,
                    details: None,
                })
                .await?;
            let commitment = response.witness.state_commitment();
            if commitment == expected_commitment {
                self.pass_through.account = deployed_account;
                info!(target: LOG_TARGET, "Batch builder collection account is deployed");
                return Ok(());
            }
            anyhow::ensure!(
                commitment == Word::empty(),
                "batch builder collection account does not match its on-chain state",
            );
            if let Some(expiration) = expiration {
                anyhow::ensure!(
                    response.block_num < expiration,
                    "batch builder collection account deployment expired",
                );
                committed_tip.changed().await.context("committed chain tip channel closed")?;
                continue;
            }
            anyhow::ensure!(
                self.pass_through.account.is_new(),
                "batch builder collection account is missing from the chain and has no creation seed",
            );

            let (header, config, blockchain) = self
                .state
                .with_view(async |view| {
                    let tip = *view.tip();
                    let (_, header, _) = view.sync_chain_mmr(tip..=tip).await?;
                    let config = view
                        .get_protocol_config(header.protocol_config_commitment())
                        .await?
                        .context("protocol configuration is missing")?;
                    let blockchain = view.get_block_inclusion_proofs(tip, BTreeSet::new()).await?;
                    anyhow::Ok((header, config, blockchain))
                })
                .await?;
            let executed = self
                .pass_through
                .execute(Vec::new(), header.clone(), config, blockchain.clone())
                .await?;
            let transaction = spawn_blocking_in_current_span(move || {
                PassThroughTransactionBuilder::prove(executed)
            })
            .await??;
            expiration = Some(transaction.expiration_block_num());
            miden_span_record!(
                transaction.id = transaction.id(),
                transaction.expires_at = transaction.expiration_block_num()
            );
            let batch = ProposedBatch::new(
                vec![Arc::new(transaction)],
                header,
                blockchain,
                BTreeMap::new(),
                MIN_PROOF_SECURITY_LEVEL,
            )?;
            let proof = self.batch_prover.prove(batch.clone()).await?;
            // This internally built batch does not use standalone transaction admission. The
            // collector's authentication procedure does not charge a deployment fee.
            api.submit_proven_tx_batch(proof, batch).await?;
            info!(target: LOG_TARGET, "Submitted batch builder collection account deployment");
        }
    }
}
