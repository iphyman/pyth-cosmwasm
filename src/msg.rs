use cosmwasm_schema::{cw_serde, QueryResponses};
use cosmwasm_std::Binary;
use pyth_sdk::{Price, PriceFeed, PriceIdentifier, UnixTimestamp};

use crate::state::PythDataSource;

type HumanAddr = String;

#[derive(Eq)]
#[cw_serde]
pub struct MigrateMsg {}
#[cw_serde]
pub struct InstantiateMsg {
    pub wormhole_contract: HumanAddr,
    pub data_sources: Vec<PythDataSource>,

    pub governance_source: PythDataSource,
    pub governance_source_index: u32,
    pub governance_sequence_number: u64,

    pub chain_id: u16,
}

#[cw_serde]
#[derive(Eq)]
pub enum ExecuteMsg {
    ExecuteGovernanceInstruction { data: Binary },
}

#[cw_serde]
#[derive(QueryResponses)]
pub enum QueryMsg {
    #[returns(ParsePriceFeedsResponse)]
    ParsePriceFeeds {
        update_data: Vec<Binary>,
        price_feed_ids: Vec<PriceIdentifier>,
        min_publish_time: UnixTimestamp,
        max_publish_time: UnixTimestamp,
    },

    #[returns(ParseSinglePriceFeedResponse)]
    ParseSinglePriceFeed {
        update_data: Binary,
        price_feed_id: PriceIdentifier,
        min_publish_time: UnixTimestamp,
        max_publish_time: UnixTimestamp,
    },
}

#[cw_serde]
pub struct ParsePriceFeedsResponse {
    pub price_feeds: Vec<PriceFeed>,
}
#[cw_serde]
pub struct ParseSinglePriceFeedResponse {
    pub price: Price,
}
