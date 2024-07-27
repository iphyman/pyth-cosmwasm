use byteorder::BigEndian;
use cosmwasm_std::{
    to_json_binary, Binary, Deps, Env, QueryRequest, StdError, StdResult, WasmQuery,
};
use cw_mini_wormhole::{msg::QueryMsg as WormholeQueryMsg, state::ParsedVAA};
use pyth_sdk::{Price, PriceFeed, PriceIdentifier};
use pyth_wormhole_attester_sdk::{BatchPriceAttestation, PriceAttestation, PriceStatus};
use pythnet_sdk::{
    accumulators::merkle::MerkleRoot,
    hashers::keccak256_160::Keccak160,
    messages::Message,
    wire::{
        from_slice,
        v1::{
            AccumulatorUpdateData, Proof, WormholeMessage, WormholePayload,
            PYTHNET_ACCUMULATOR_UPDATE_MAGIC,
        },
    },
};

use crate::{
    state::{ConfigInfo, PythDataSource, CONFIG},
    ContractError,
};

/// Check that `vaa` is from a valid data source (and hence is a legitimate price update message).
pub fn verify_vaa_from_data_source(state: &ConfigInfo, vaa: &ParsedVAA) -> StdResult<()> {
    let vaa_data_source = PythDataSource {
        emitter: vaa.emitter_address.clone().into(),
        chain_id: vaa.emitter_chain,
    };

    if !state.data_sources.contains(&vaa_data_source) {
        Err(StdError::generic_err("Invalid update emitter"))?
    }

    Ok(())
}

/// Check that `vaa` is from a valid governance source (and hence is a legitimate governance instruction).
pub fn verify_vaa_from_governance_source(
    state: &ConfigInfo,
    vaa: &ParsedVAA,
) -> Result<(), ContractError> {
    let vaa_data_source = PythDataSource {
        emitter: vaa.emitter_address.clone().into(),
        chain_id: vaa.emitter_chain,
    };

    if state.governance_source != vaa_data_source {
        return Err(ContractError::InvalidUpdateEmitter {});
    }

    Ok(())
}

/// Verify that `data` represents an authentic Wormhole VAA.
///
/// *Warning* this function does not verify the emitter of the wormhole message; it only checks
/// that the wormhole signatures are valid. The caller is responsible for checking that the message
/// originates from the expected emitter.
pub fn parse_and_verify_vaa(deps: Deps, block_time: u64, data: Binary) -> StdResult<ParsedVAA> {
    let cfg = CONFIG.load(deps.storage)?;
    let vaa: ParsedVAA = deps.querier.query(&QueryRequest::Wasm(WasmQuery::Smart {
        contract_addr: cfg.wormhole_contract.to_string(),
        msg: to_json_binary(&WormholeQueryMsg::VerifyVAA {
            vaa: data,
            block_time,
        })?,
    }))?;

    Ok(vaa)
}

pub fn parse_update(deps: &Deps, env: &Env, data: &Binary) -> StdResult<Vec<PriceFeed>> {
    let header = data.get(0..4);
    let feeds = if header == Some(PYTHNET_ACCUMULATOR_UPDATE_MAGIC.as_slice()) {
        parse_accumulator(deps, env, data)?
    } else {
        parse_batch_attestation(deps, env, data)?
    };

    Ok(feeds)
}

fn parse_accumulator(deps: &Deps, env: &Env, data: &[u8]) -> StdResult<Vec<PriceFeed>> {
    let update_data = AccumulatorUpdateData::try_from_slice(data)
        .map_err(|_| StdError::generic_err("Invalid accumalator payload"))?;

    match update_data.proof {
        Proof::WormholeMerkle { vaa, updates } => {
            let parsed_vaa = parse_and_verify_vaa(
                *deps,
                env.block.time.seconds(),
                Binary::from(Vec::from(vaa)),
            )?;
            let config = CONFIG.load(deps.storage)?;
            verify_vaa_from_data_source(&config, &parsed_vaa)?;

            let msg = WormholeMessage::try_from_bytes(parsed_vaa.payload)
                .map_err(|_| StdError::generic_err("Invalid wormhole message"))?;

            let root: MerkleRoot<Keccak160> = MerkleRoot::new(match msg.payload {
                WormholePayload::Merkle(merkle_root) => merkle_root.root,
            });
            let mut feeds = vec![];
            for update in updates {
                let message_vec = Vec::from(update.message);
                if !root.check(update.proof, &message_vec) {
                    return Err(StdError::generic_err("Invalid merkly proof"));
                }

                let msg = from_slice::<BigEndian, Message>(&message_vec)
                    .map_err(|_| StdError::generic_err("Invalid accumulator message"))?;

                match msg {
                    Message::PriceFeedMessage(price_feed_message) => {
                        let price_feed = PriceFeed::new(
                            PriceIdentifier::new(price_feed_message.feed_id),
                            Price {
                                price: price_feed_message.price,
                                conf: price_feed_message.conf,
                                expo: price_feed_message.exponent,
                                publish_time: price_feed_message.publish_time,
                            },
                            Price {
                                price: price_feed_message.ema_price,
                                conf: price_feed_message.ema_conf,
                                expo: price_feed_message.exponent,
                                publish_time: price_feed_message.publish_time,
                            },
                        );
                        feeds.push(price_feed);
                    }
                    _ => return Err(StdError::generic_err("Invalid accumulator message type"))?,
                }
            }
            Ok(feeds)
        }
    }
}

/// Update the on-chain storage for any new price updates provided in `batch_attestation`.
fn parse_batch_attestation(deps: &Deps, env: &Env, data: &Binary) -> StdResult<Vec<PriceFeed>> {
    let vaa = parse_and_verify_vaa(*deps, env.block.time.seconds(), data.clone())?;
    let config = CONFIG.load(deps.storage)?;

    verify_vaa_from_data_source(&config, &vaa)?;

    let data = &vaa.payload;
    let batch_attestation = BatchPriceAttestation::deserialize(&data[..])
        .map_err(|_| StdError::generic_err("Invalid update payload"))?;
    let mut feeds = vec![];

    // Update prices
    for price_attestation in batch_attestation.price_attestations.iter() {
        let price_feed = create_price_feed_from_price_attestation(price_attestation);
        feeds.push(price_feed);
    }

    Ok(feeds)
}

fn create_price_feed_from_price_attestation(price_attestation: &PriceAttestation) -> PriceFeed {
    match price_attestation.status {
        PriceStatus::Trading => PriceFeed::new(
            PriceIdentifier::new(price_attestation.price_id.to_bytes()),
            Price {
                price: price_attestation.price,
                conf: price_attestation.conf,
                expo: price_attestation.expo,
                publish_time: price_attestation.publish_time,
            },
            Price {
                price: price_attestation.ema_price,
                conf: price_attestation.ema_conf,
                expo: price_attestation.expo,
                publish_time: price_attestation.publish_time,
            },
        ),
        _ => PriceFeed::new(
            PriceIdentifier::new(price_attestation.price_id.to_bytes()),
            Price {
                price: price_attestation.prev_price,
                conf: price_attestation.prev_conf,
                expo: price_attestation.expo,
                publish_time: price_attestation.prev_publish_time,
            },
            Price {
                price: price_attestation.ema_price,
                conf: price_attestation.ema_conf,
                expo: price_attestation.expo,
                publish_time: price_attestation.prev_publish_time,
            },
        ),
    }
}
