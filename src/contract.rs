use std::collections::HashSet;

#[cfg(not(feature = "library"))]
use cosmwasm_std::entry_point;
use cosmwasm_std::{
    to_json_binary, Binary, Deps, DepsMut, Env, MessageInfo, Response, StdError, StdResult,
};
use cw2::set_contract_version;
use pyth_sdk::{Identifier, Price, PriceFeed, PriceIdentifier, UnixTimestamp};

use crate::error::ContractError;
use crate::governance::{GovernanceAction, GovernanceInstruction, GovernanceModule};
use crate::helpers::{parse_and_verify_vaa, parse_update, verify_vaa_from_governance_source};
use crate::msg::{
    ExecuteMsg, InstantiateMsg, ParsePriceFeedsResponse, ParseSinglePriceFeedResponse, QueryMsg,
};
use crate::state::{ConfigInfo, CONFIG};

// version info for migration info
const CONTRACT_NAME: &str = "crates.io:pyth_cosmwasm";
const CONTRACT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn instantiate(
    deps: DepsMut,
    _env: Env,
    _info: MessageInfo,
    msg: InstantiateMsg,
) -> Result<Response, ContractError> {
    // Save general wormhole and pyth info
    let config = ConfigInfo {
        wormhole_contract: deps.api.addr_validate(msg.wormhole_contract.as_ref())?,
        data_sources: msg.data_sources.iter().cloned().collect(),
        chain_id: msg.chain_id,
        governance_source: msg.governance_source.clone(),
        governance_source_index: msg.governance_source_index,
        governance_sequence_number: msg.governance_sequence_number,
    };

    CONFIG.save(deps.storage, &config)?;
    set_contract_version(deps.storage, CONTRACT_NAME, CONTRACT_VERSION)?;

    Ok(Response::default())
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn execute(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    msg: ExecuteMsg,
) -> Result<Response, ContractError> {
    match msg {
        ExecuteMsg::ExecuteGovernanceInstruction { data } => {
            execute_governance_instruction(deps, env, info, &data)
        }
    }
}

/// Execute a governance instruction provided as the VAA `data`.
/// The VAA must come from an authorized governance emitter.
/// See [GovernanceInstruction] for descriptions of the supported operations.
fn execute_governance_instruction(
    deps: DepsMut,
    env: Env,
    _info: MessageInfo,
    data: &Binary,
) -> Result<Response, ContractError> {
    let vaa = parse_and_verify_vaa(deps.as_ref(), env.block.time.seconds(), data.clone())?;
    let config = CONFIG.load(deps.storage)?;

    verify_vaa_from_governance_source(&config, &vaa)?;

    // store updates to the config as a result of this action in here.
    let mut updated_config: ConfigInfo = config.clone();

    // Governance messages must be applied in order. This check prevents replay attacks where
    // previous messages are re-applied.
    if vaa.sequence <= config.governance_sequence_number {
        Err(ContractError::OldGovernanceMessage {})?
    } else {
        updated_config.governance_sequence_number = vaa.sequence;
    }

    let data = &vaa.payload;
    let instruction = GovernanceInstruction::deserialize(&data[..])
        .map_err(|_| ContractError::InvalidGovernancePayload {})?;

    // Check that the instruction is intended for this chain.
    // chain_id = 0 means the instruction applies to all chains
    if instruction.target_chain_id != config.chain_id && instruction.target_chain_id != 0 {
        Err(ContractError::InvalidGovernancePayload {})?
    }

    // Check that the instruction is intended for this target chain contract (as opposed to
    // other Pyth contracts that may live on the same chain).
    if instruction.module != GovernanceModule::Target {
        Err(ContractError::InvalidGovernancePayload {})?
    }

    let response = match instruction.action {
        GovernanceAction::SetDataSources { data_sources } => {
            updated_config.data_sources = HashSet::from_iter(data_sources.iter().cloned());

            Response::new()
                .add_attribute("action", "set_data_sources")
                .add_attribute("new_data_sources", format!("{data_sources:?}"))
        }
    };

    CONFIG.save(deps.storage, &updated_config)?;

    Ok(response)
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn query(deps: Deps, env: Env, msg: QueryMsg) -> StdResult<Binary> {
    match msg {
        QueryMsg::ParsePriceFeeds {
            update_data,
            price_feed_ids,
            min_publish_time,
            max_publish_time,
        } => to_json_binary(&query_parse_price_feed_updates(
            deps,
            &env,
            &update_data,
            price_feed_ids,
            min_publish_time,
            max_publish_time,
        )?),

        QueryMsg::ParseSinglePriceFeed {
            update_data,
            price_feed_id,
            min_publish_time,
            max_publish_time,
        } => to_json_binary(&query_parse_single_price_feed_update(
            deps,
            &env,
            &update_data,
            price_feed_id,
            min_publish_time,
            max_publish_time,
        )?),
    }
}

pub fn query_parse_price_feed_updates(
    deps: Deps,
    env: &Env,
    updates: &[Binary],
    price_feeds: Vec<PriceIdentifier>,
    min_publish_time: UnixTimestamp,
    max_publish_time: UnixTimestamp,
) -> StdResult<ParsePriceFeedsResponse> {
    let mut found_feeds = 0;
    let mut results: Vec<(Identifier, Option<PriceFeed>)> =
        price_feeds.iter().map(|id| (*id, None)).collect();

    for datum in updates {
        let feeds = parse_update(&deps, env, datum)?;

        for result in results.as_mut_slice() {
            if result.1.is_some() {
                continue;
            }

            for feed in feeds.as_slice() {
                if feed.get_price_unchecked().publish_time < min_publish_time
                    || feed.get_price_unchecked().publish_time > max_publish_time
                {
                    continue;
                }

                if result.0 == feed.id {
                    result.1 = Some(*feed);
                    found_feeds += 1;
                    break;
                }
            }
        }
    }

    if found_feeds != price_feeds.len() {
        Err(StdError::generic_err("Invalid update data"))?
    }

    let unwrapped_feeds = results
        .into_iter()
        .map(|(_, feed)| feed.unwrap())
        .collect::<Vec<PriceFeed>>();

    Ok(ParsePriceFeedsResponse {
        price_feeds: unwrapped_feeds,
    })
}

pub fn query_parse_single_price_feed_update(
    deps: Deps,
    env: &Env,
    update_data: &Binary,
    price_feed: PriceIdentifier,
    min_publish_time: UnixTimestamp,
    max_publish_time: UnixTimestamp,
) -> StdResult<ParseSinglePriceFeedResponse> {
    let mut price = Price::default();
    let feeds = parse_update(&deps, env, update_data)?;

    for feed in feeds {
        let feed_price = feed.get_price_unchecked();
        if feed.id == price_feed
            && feed_price.publish_time > min_publish_time
            && feed_price.publish_time < max_publish_time
        {
            price = feed_price;
            break;
        } else {
            Err(StdError::generic_err("Price not found within range"))?
        }
    }

    Ok(ParseSinglePriceFeedResponse { price })
}

#[cfg(test)]
mod tests {}
