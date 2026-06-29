use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct InstantiateMsg {}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecuteMsg {
    Ping {
        destination_port: String,
    },
    ReceivePacket {
        source_port: String,
        destination_port: String,
        sequence: u64,
        msg: String,
    },
    AcknowledgePacket {
        source_port: String,
        destination_port: String,
        sequence: u64,
    },
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QueryMsg {
    Stats {},
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct StatsResponse {
    pub pings_sent: u64,
    pub pongs_received: u64,
    pub next_sequence: u64,
}
