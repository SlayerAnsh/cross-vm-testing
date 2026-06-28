use cosmwasm_std::{Deps, DepsMut, Env, Response, StdResult, ensure};

use crate::events::{
    PING_MSG, PONG_ACK, acknowledge_packet_event, receive_packet_event, send_packet_event,
    write_acknowledgement_event,
};
use crate::msg::{ExecuteMsg, InstantiateMsg, QueryMsg, StatsResponse};
use crate::port::format_port;
use crate::state::{NEXT_SEQUENCE, PINGS_SENT, PONGS_RECEIVED};

fn self_port(env: &Env) -> String {
    format_port(&env.block.chain_id, env.contract.address.as_str())
}

pub fn instantiate(deps: DepsMut, _msg: InstantiateMsg) -> StdResult<Response> {
    PINGS_SENT.save(deps.storage, &0u64)?;
    PONGS_RECEIVED.save(deps.storage, &0u64)?;
    NEXT_SEQUENCE.save(deps.storage, &0u64)?;

    Ok(Response::new().add_attribute("action", "instantiate"))
}

pub fn execute(deps: DepsMut, env: Env, msg: ExecuteMsg) -> StdResult<Response> {
    match msg {
        ExecuteMsg::Ping { destination_port } => ping(deps, env, destination_port),
        ExecuteMsg::ReceivePacket {
            source_port,
            destination_port,
            sequence,
            msg,
        } => receive_packet(deps, env, source_port, destination_port, sequence, msg),
        ExecuteMsg::AcknowledgePacket {
            source_port,
            destination_port,
            sequence,
        } => acknowledge_packet(deps, source_port, destination_port, sequence),
    }
}

fn ping(deps: DepsMut, env: Env, destination_port: String) -> StdResult<Response> {
    let source_port = self_port(&env);
    let sequence = NEXT_SEQUENCE.load(deps.storage)?;

    let mut pings_sent = PINGS_SENT.load(deps.storage)?;
    pings_sent += 1;
    PINGS_SENT.save(deps.storage, &pings_sent)?;

    let next_sequence = sequence + 1;
    NEXT_SEQUENCE.save(deps.storage, &next_sequence)?;

    Ok(Response::new()
        .add_attribute("action", "ping")
        .add_event(send_packet_event(
            &source_port,
            &destination_port,
            sequence,
            PING_MSG,
        )))
}

fn receive_packet(
    _deps: DepsMut,
    env: Env,
    source_port: String,
    destination_port: String,
    sequence: u64,
    msg: String,
) -> StdResult<Response> {
    ensure!(
        destination_port == self_port(&env),
        cosmwasm_std::StdError::generic_err("invalid destination port")
    );
    ensure!(
        msg == PING_MSG,
        cosmwasm_std::StdError::generic_err("invalid packet message")
    );

    Ok(Response::new()
        .add_attribute("action", "receive_packet")
        .add_event(receive_packet_event(
            &source_port,
            &destination_port,
            sequence,
        ))
        .add_event(write_acknowledgement_event(
            &source_port,
            &destination_port,
            sequence,
            PING_MSG,
            PONG_ACK,
        )))
}

fn acknowledge_packet(
    deps: DepsMut,
    source_port: String,
    destination_port: String,
    sequence: u64,
) -> StdResult<Response> {
    let mut pongs_received = PONGS_RECEIVED.load(deps.storage)?;
    pongs_received += 1;
    PONGS_RECEIVED.save(deps.storage, &pongs_received)?;

    Ok(Response::new()
        .add_attribute("action", "acknowledge_packet")
        .add_event(acknowledge_packet_event(
            &source_port,
            &destination_port,
            sequence,
        )))
}

pub fn query(deps: Deps, msg: QueryMsg) -> StdResult<StatsResponse> {
    match msg {
        QueryMsg::Stats {} => Ok(StatsResponse {
            pings_sent: PINGS_SENT.load(deps.storage)?,
            pongs_received: PONGS_RECEIVED.load(deps.storage)?,
            next_sequence: NEXT_SEQUENCE.load(deps.storage)?,
        }),
    }
}
