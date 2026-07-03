# Cross-Chain Ping-Pong Examples

IBC-style ping-pong contracts in CosmWasm, Solidity, and Solana. Each contract emits the same four packet lifecycle events so a future cross-VM relayer can treat them uniformly.

## Port format

```
{chain_id}.{address}
```

| VM | `chain_id` source | `address` source |
| --- | --- | --- |
| CosmWasm | `env.block.chain_id` | contract address (bech32) |
| EVM | constructor arg | `address(this)` (0x-prefixed hex) |
| Solana | `initialize` arg | program id (base58) |

Examples:

- `osmosis-1.osmo1abc...`
- `1.0x742d35Cc6634C0532925a3b844Bc9e7595f0bEb`
- `mainnet-beta.BpongPong7TSiNYMVrY4CtSzE4MjAWzhNGpYWs9m2kdaFPu`

## Event schema

| Event | Fields |
| --- | --- |
| `SendPacket` | `source_port`, `destination_port`, `packet_sequence`, `msg` |
| `ReceivePacket` | `source_port`, `destination_port`, `packet_sequence` |
| `WriteAcknowledgement` | `source_port`, `destination_port`, `packet_sequence`, `msg`, `ack` |
| `AcknowledgePacket` | `source_port`, `destination_port`, `packet_sequence` |

Constants: `msg = "ping"`, `ack = "pong"`.

## Execute API

| Call | Who invokes | Emits |
| --- | --- | --- |
| `ping(destination_port)` | User | `SendPacket` |
| `receive_packet(source_port, destination_port, sequence, msg)` | Relayer | `ReceivePacket`, `WriteAcknowledgement` |
| `acknowledge_packet(source_port, destination_port, sequence)` | Relayer | `AcknowledgePacket` |

## Query / view stats

- `pings_sent`
- `pongs_received`
- `next_sequence` (next outbound packet sequence)

## Packet lifecycle

```
User → ContractA: ping(dest_port_B)
ContractA → emit SendPacket(msg="ping")

Relayer → ContractB: receive_packet(src_A, dest_B, seq, "ping")
ContractB → emit ReceivePacket
ContractB → emit WriteAcknowledgement(ack="pong")

Relayer → ContractA: acknowledge_packet(src_A, dest_B, seq)
ContractA → emit AcknowledgePacket
ContractA → pongs_received += 1
```

## Build commands

```bash
# CosmWasm
cd contracts/cosmwasm/ping-pong && make build

# Solidity
cd contracts/solidity && forge build && forge test

# Solana
cd contracts/solana && anchor build
```

## Manual relay walkthrough

Pseudocode for relaying a ping from chain A to chain B:

```text
1. User calls ping(dest_port_B) on contract A.
2. Relayer reads SendPacket event:
     source_port      = port_A
     destination_port = port_B
     packet_sequence  = seq
     msg              = "ping"
3. Relayer calls receive_packet(port_A, port_B, seq, "ping") on contract B.
4. Relayer reads WriteAcknowledgement event (ack = "pong").
5. Relayer calls acknowledge_packet(port_A, port_B, seq) on contract A.
6. Query contract A stats: pongs_received should be 1.
```

Foundry test `PingPong.t.sol` demonstrates steps 1–6 with two deployed `PingPong` instances on the same chain (chain ids `"1"` and `"42161"`).

The cross-VM relayer test `examples/cross-vm-tests/tests/cross_vm/ping_pong.rs` demonstrates the same lifecycle across heterogeneous chains. It deploys a `PingPong` on a CosmWasm, an EVM, and a Solana chain in one `MultiChainEnv`, parses packet events through each contract's `on_after` hook into a shared `BridgeLedger`, and uses a `Bridge` (which has access to the env) to move each packet to its destination port's chain and contract. It covers every ordered VM pair (running the Solana arm needs `make compile-solana`).

## Example locations

| VM | Path |
| --- | --- |
| CosmWasm | `contracts/cosmwasm/ping-pong/` |
| Solidity | `contracts/solidity/src/PingPong.sol` |
| Solana | `contracts/solana/programs/ping-pong/` |
