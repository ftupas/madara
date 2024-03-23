use jsonrpsee::core::RpcResult;
use log::error;
use mc_genesis_data_provider::GenesisProvider;
use mp_felt::Felt252Wrapper;
use mp_hashers::HasherT;
use pallet_starknet_runtime_api::{ConvertTransactionRuntimeApi, StarknetRuntimeApi};
use sc_client_api::backend::{Backend, StorageProvider};
use sc_client_api::BlockBackend;
use sc_transaction_pool::ChainApi;
use sc_transaction_pool_api::TransactionPool;
use sp_api::ProvideRuntimeApi;
use sp_blockchain::HeaderBackend;
use sp_runtime::traits::Block as BlockT;
use starknet_api::transaction::Calldata;
use starknet_core::types::{BlockId, FunctionCall};

use super::tools::convert_error;
use crate::errors::StarknetRpcApiError;
use crate::{Arc, Starknet};

/// Call a Function in a Contract Without Creating a Transaction
///
/// ### Arguments
///
/// * `request` - The details of the function call to be made. This includes information such as the
///   contract address, function signature, and arguments.
/// * `block_id` - The identifier of the block used to reference the state or call the transaction
///   on. This can be the hash of the block, its number (height), or a specific block tag.
///
/// ### Returns
///
/// * `result` - The function's return value, as defined in the Cairo output. This is an array of
///   field elements (`Felt`).
///
/// ### Errors
///
/// This method may return the following errors:
/// * `CONTRACT_NOT_FOUND` - If the specified contract address does not exist.
/// * `CONTRACT_ERROR` - If there is an error with the contract or the function call.
/// * `BLOCK_NOT_FOUND` - If the specified block does not exist in the blockchain.
pub fn call<A, B, BE, G, C, P, H>(
    starknet: &Starknet<A, B, BE, G, C, P, H>,
    request: FunctionCall,
    block_id: BlockId,
) -> RpcResult<Vec<String>>
where
    A: ChainApi<Block = B> + 'static,
    B: BlockT,
    P: TransactionPool<Block = B> + 'static,
    BE: Backend<B> + 'static,
    C: HeaderBackend<B> + BlockBackend<B> + StorageProvider<B, BE> + 'static,
    C: ProvideRuntimeApi<B>,
    C::Api: StarknetRuntimeApi<B> + ConvertTransactionRuntimeApi<B>,
    G: GenesisProvider + Send + Sync + 'static,
    H: HasherT + Send + Sync + 'static,
{
    let substrate_block_hash = starknet.substrate_block_hash_from_starknet_block(block_id).map_err(|e| {
        error!("'{e}'");
        StarknetRpcApiError::BlockNotFound
    })?;

    let runtime_api = starknet.client.runtime_api();

    let calldata = Calldata(Arc::new(request.calldata.iter().map(|x| Felt252Wrapper::from(*x).into()).collect()));

    let result = runtime_api
        .call(
            substrate_block_hash,
            Felt252Wrapper(request.contract_address).into(),
            Felt252Wrapper(request.entry_point_selector).into(),
            calldata,
        )
        .map_err(|e| {
            error!("Request parameters error: {e}");
            StarknetRpcApiError::InternalServerError
        })?;

    let result = convert_error(starknet.client.clone(), substrate_block_hash, result)?;

    Ok(result.iter().map(|x| format!("{:#x}", x.0)).collect())
}
