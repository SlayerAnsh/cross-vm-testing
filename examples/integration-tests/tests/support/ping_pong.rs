//! Cross-VM `PingPong` wrapper: one logical interface (`ping` / `receive_packet` /
//! `acknowledge_packet` / `stats` / `port`) implemented on CosmWasm, EVM, and Solana, so the
//! [`Bridge`](super::bridge::Bridge) can relay packets between any pair of them.
//!
//! Mirrors `support/counter.rs`: `#[cross_vm_contract(PingPong)]` generates the struct,
//! constructors, hook forwarders, and the VM-dispatching `impl PingPongSpec`. Only the
//! per-VM `cw_*` / `evm_*` / `svm_*` hooks below are hand-written. The three packet-emitting
//! methods return `AppResponse<()>`, so they fire before/after hooks; `stats` and `port` are
//! plain reads.

use cross_vm_framework::prelude::*;

use cross_vm_solana::Address as SvmAddress;
use cross_vm_solidity::Bytes;
use solana_instruction::{AccountMeta, Instruction};

use super::bridge::{read_string, read_u64, write_string, write_u64};

// ----- CosmWasm: the canonical ping-pong contract from `examples/`, wrapped in-process. -----
mod cosmos_pp {
    use cosmwasm_std::Empty;
    use cw_multi_test::{Contract, ContractWrapper};

    pub use ping_pong::{ExecuteMsg, InstantiateMsg, QueryMsg, StatsResponse};

    pub fn contract() -> Box<dyn Contract<Empty, Empty>> {
        Box::new(ContractWrapper::new(
            ping_pong::execute,
            ping_pong::instantiate,
            ping_pong::query,
        ))
    }
}

// ----- EVM: ABI + creation bytecode from the forge artifact. Public so the bridge's event
// parser can reuse the generated `SolEvent` types. -----
pub mod evm_pp {
    alloy::sol!(
        #[sol(abi)]
        PingPong,
        "../solidity-contracts/out/PingPong.sol/PingPong.json"
    );
}

// ----- Solana: the Anchor ping-pong program, its id, and instruction discriminators. -----
const SOLANA_PROGRAM_ID: &str = "54ex8sgs6H3Y2NssU3CWdBhySk9q5Gqc4MMtPYTtJzC5";
/// `sha256("global:initialize")[..8]`.
const DISC_INITIALIZE: [u8; 8] = [175, 175, 109, 31, 13, 152, 155, 237];
/// `sha256("global:ping")[..8]`.
const DISC_PING: [u8; 8] = [173, 0, 94, 236, 73, 133, 225, 153];
/// `sha256("global:receive_packet")[..8]`.
const DISC_RECEIVE_PACKET: [u8; 8] = [63, 80, 211, 98, 33, 16, 172, 29];
/// `sha256("global:acknowledge_packet")[..8]`.
const DISC_ACKNOWLEDGE_PACKET: [u8; 8] = [232, 102, 184, 27, 48, 4, 54, 252];
/// Built by `make compile-solana` (`anchor build` / `cargo-build-sbf`).
const PING_PONG_SO: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../solana-contracts/target/deploy/ping_pong.so"
));

/// A VM-agnostic snapshot of a ping-pong contract's counters.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct StatsView {
    pub pings_sent: u64,
    pub pongs_received: u64,
    pub next_sequence: u64,
}

/// The cross-VM ping-pong contract's logical methods. `#[cross_vm_contract(PingPong)]` turns
/// this into the `PingPong` wrapper.
///
/// `ping` / `receive_packet` / `acknowledge_packet` return `AppResponse`, so their dispatchers
/// fire before/after hooks (where the relayer records emitted packets); `stats` and `port` are
/// plain reads.
#[cross_vm_contract(PingPong)]
pub trait PingPongSpec {
    /// Deploy the contract (and, on Solana, `initialize` its per-user state) under `chain_id`,
    /// signed by `wallet`.
    async fn setup(&self, wallet: &str, chain_id: &str);
    /// Send a ping to `destination_port` (emits `SendPacket`), signed by `wallet`.
    async fn ping(&self, wallet: &str, destination_port: String) -> AppResponse<()>;
    /// Relay-side: accept a packet (emits `ReceivePacket` + `WriteAcknowledgement`).
    async fn receive_packet(
        &self,
        wallet: &str,
        source_port: String,
        destination_port: String,
        sequence: u64,
        msg: String,
    ) -> AppResponse<()>;
    /// Relay-side: acknowledge a packet back on the source (emits `AcknowledgePacket`).
    async fn acknowledge_packet(
        &self,
        wallet: &str,
        source_port: String,
        destination_port: String,
        sequence: u64,
    ) -> AppResponse<()>;
    /// Read the contract's counters.
    async fn stats(&self) -> StatsView;
    /// The contract's own port (`"{chain_id}.{address}"`), exactly as it validates incoming
    /// packets against.
    async fn port(&self) -> String;
}

impl PingPong {
    // ----- CosmWasm hooks -----
    async fn cw_setup(&self, wallet: &str, _chain_id: &str) -> Result<(), CrossVmError> {
        // CosmWasm derives its port from the runtime block chain_id, not a constructor arg.
        let chain = self.base.cosmwasm()?;
        let code_id = chain.store_code(cosmos_pp::contract()).await?;
        let addr = chain
            .instantiate(
                code_id,
                cosmos_pp::InstantiateMsg {},
                WalletLabel::wrap(wallet),
                &[],
                "ping-pong",
            )
            .await?;
        self.base.set_address(Account::CosmWasm(addr));
        Ok(())
    }

    async fn cw_ping(
        &self,
        wallet: &str,
        destination_port: String,
    ) -> Result<AppResponse<()>, CrossVmError> {
        let raw = self
            .base
            .cosmwasm()?
            .contract(self.base.cw_addr()?)
            .execute(cosmos_pp::ExecuteMsg::Ping { destination_port }, wallet)
            .await?;
        Ok(AppResponse::cosmwasm((), raw))
    }

    async fn cw_receive_packet(
        &self,
        wallet: &str,
        source_port: String,
        destination_port: String,
        sequence: u64,
        msg: String,
    ) -> Result<AppResponse<()>, CrossVmError> {
        let raw = self
            .base
            .cosmwasm()?
            .contract(self.base.cw_addr()?)
            .execute(
                cosmos_pp::ExecuteMsg::ReceivePacket {
                    source_port,
                    destination_port,
                    sequence,
                    msg,
                },
                wallet,
            )
            .await?;
        Ok(AppResponse::cosmwasm((), raw))
    }

    async fn cw_acknowledge_packet(
        &self,
        wallet: &str,
        source_port: String,
        destination_port: String,
        sequence: u64,
    ) -> Result<AppResponse<()>, CrossVmError> {
        let raw = self
            .base
            .cosmwasm()?
            .contract(self.base.cw_addr()?)
            .execute(
                cosmos_pp::ExecuteMsg::AcknowledgePacket {
                    source_port,
                    destination_port,
                    sequence,
                },
                wallet,
            )
            .await?;
        Ok(AppResponse::cosmwasm((), raw))
    }

    async fn cw_stats(&self) -> Result<StatsView, CrossVmError> {
        let resp: cosmos_pp::StatsResponse = self
            .base
            .cosmwasm()?
            .contract(self.base.cw_addr()?)
            .query(cosmos_pp::QueryMsg::Stats {})
            .await?;
        Ok(StatsView {
            pings_sent: resp.pings_sent,
            pongs_received: resp.pongs_received,
            next_sequence: resp.next_sequence,
        })
    }

    async fn cw_port(&self) -> Result<String, CrossVmError> {
        let addr = self.base.cw_addr()?;
        // The contract's `self_port` uses `env.block.chain_id`; on the cw-multi-test mock that
        // is the runtime default, not the chain spec id. Read the live value so the port we
        // hand out matches what `receive_packet` checks against.
        let chain_id = match self.base.cosmwasm()? {
            CwChain::Mock(p) => p.app().block_info().chain_id,
            _ => {
                return Err(CrossVmError::unsupported(
                    ChainKind::CosmWasm,
                    "cw rpc port",
                ));
            }
        };
        Ok(format!("{chain_id}.{addr}"))
    }

    // ----- EVM hooks -----
    async fn evm_setup(&self, wallet: &str, chain_id: &str) -> Result<(), CrossVmError> {
        use alloy::sol_types::SolConstructor;
        let chain = self.base.evm()?;
        let args = evm_pp::PingPong::constructorCall {
            _chainId: chain_id.to_string(),
        }
        .abi_encode();
        let addr = chain
            .deploy_create(
                evm_pp::PingPong::BYTECODE.clone(),
                args,
                WalletLabel::wrap(wallet),
            )
            .await?;
        self.base.set_address(Account::Evm(addr));
        Ok(())
    }

    async fn evm_ping(
        &self,
        wallet: &str,
        destination_port: String,
    ) -> Result<AppResponse<()>, CrossVmError> {
        use alloy::sol_types::SolCall;
        let chain = self.base.evm()?;
        let calldata = Bytes::from(
            evm_pp::PingPong::pingCall {
                destinationPort: destination_port,
            }
            .abi_encode(),
        );
        let exec = chain
            .call(&self.base.evm_addr()?, calldata, WalletLabel::wrap(wallet))
            .await?;
        Ok(AppResponse::evm((), exec.output, exec.logs))
    }

    async fn evm_receive_packet(
        &self,
        wallet: &str,
        source_port: String,
        destination_port: String,
        sequence: u64,
        msg: String,
    ) -> Result<AppResponse<()>, CrossVmError> {
        use alloy::sol_types::SolCall;
        let chain = self.base.evm()?;
        let calldata = Bytes::from(
            evm_pp::PingPong::receivePacketCall {
                sourcePort: source_port,
                destinationPort: destination_port,
                sequence,
                packetMsg: Bytes::from(msg.into_bytes()),
            }
            .abi_encode(),
        );
        let exec = chain
            .call(&self.base.evm_addr()?, calldata, WalletLabel::wrap(wallet))
            .await?;
        Ok(AppResponse::evm((), exec.output, exec.logs))
    }

    async fn evm_acknowledge_packet(
        &self,
        wallet: &str,
        source_port: String,
        destination_port: String,
        sequence: u64,
    ) -> Result<AppResponse<()>, CrossVmError> {
        use alloy::sol_types::SolCall;
        let chain = self.base.evm()?;
        let calldata = Bytes::from(
            evm_pp::PingPong::acknowledgePacketCall {
                sourcePort: source_port,
                destinationPort: destination_port,
                sequence,
            }
            .abi_encode(),
        );
        let exec = chain
            .call(&self.base.evm_addr()?, calldata, WalletLabel::wrap(wallet))
            .await?;
        Ok(AppResponse::evm((), exec.output, exec.logs))
    }

    async fn evm_stats(&self) -> Result<StatsView, CrossVmError> {
        use alloy::sol_types::SolCall;
        let chain = self.base.evm()?;
        let addr = self.base.evm_addr()?;
        let read_u64 = |out: &[u8]| -> Result<u64, CrossVmError> {
            // All three getters return a single `uint64`.
            evm_pp::PingPong::pingsSentCall::abi_decode_returns(out).map_err(|e| {
                CrossVmError::Query {
                    kind: ChainKind::Evm,
                    reason: e.to_string(),
                }
            })
        };
        let pings = chain
            .static_call(
                &addr,
                Bytes::from(evm_pp::PingPong::pingsSentCall {}.abi_encode()),
            )
            .await?;
        let pongs = chain
            .static_call(
                &addr,
                Bytes::from(evm_pp::PingPong::pongsReceivedCall {}.abi_encode()),
            )
            .await?;
        let next = chain
            .static_call(
                &addr,
                Bytes::from(evm_pp::PingPong::nextSequenceCall {}.abi_encode()),
            )
            .await?;
        Ok(StatsView {
            pings_sent: read_u64(&pings)?,
            pongs_received: read_u64(&pongs)?,
            next_sequence: read_u64(&next)?,
        })
    }

    async fn evm_port(&self) -> Result<String, CrossVmError> {
        use alloy::sol_types::SolCall;
        let chain = self.base.evm()?;
        let out = chain
            .static_call(
                &self.base.evm_addr()?,
                Bytes::from(evm_pp::PingPong::selfPortCall {}.abi_encode()),
            )
            .await?;
        evm_pp::PingPong::selfPortCall::abi_decode_returns(&out).map_err(|e| CrossVmError::Query {
            kind: ChainKind::Evm,
            reason: e.to_string(),
        })
    }

    // ----- Solana hooks -----
    async fn svm_setup(&self, wallet: &str, chain_id: &str) -> Result<(), CrossVmError> {
        let chain = self.base.solana()?;
        let program_id = SvmAddress::from_str_const(SOLANA_PROGRAM_ID);
        chain
            .add_program_at(program_id, PING_PONG_SO.to_vec())
            .await?;
        let user = chain.wallet_address(WalletLabel::wrap(wallet)).await?;
        let (pda, _bump) =
            SvmAddress::find_program_address(&[b"ping_pong", user.as_ref()], &program_id);

        let mut data = DISC_INITIALIZE.to_vec();
        write_string(&mut data, chain_id);
        let ix = Instruction::new_with_bytes(
            program_id,
            &data,
            vec![
                AccountMeta::new(pda, false),
                AccountMeta::new(user, true),
                AccountMeta::new_readonly(solana_system_interface::program::ID, false),
            ],
        );
        chain
            .send_transaction(vec![ix], WalletLabel::wrap(wallet))
            .await?;
        self.base.set_address(Account::Svm(pda));
        Ok(())
    }

    /// Build an `Update`-context instruction (`ping`/`receive_packet`/`acknowledge_packet`):
    /// the per-user state PDA (writable) plus the user (signer).
    async fn svm_update_ix(
        &self,
        wallet: &str,
        data: Vec<u8>,
    ) -> Result<Instruction, CrossVmError> {
        let chain = self.base.solana()?;
        let program_id = SvmAddress::from_str_const(SOLANA_PROGRAM_ID);
        let pda = self.base.svm_addr()?;
        let user = chain.wallet_address(WalletLabel::wrap(wallet)).await?;
        Ok(Instruction::new_with_bytes(
            program_id,
            &data,
            vec![
                AccountMeta::new(pda, false),
                AccountMeta::new_readonly(user, true),
            ],
        ))
    }

    async fn svm_ping(
        &self,
        wallet: &str,
        destination_port: String,
    ) -> Result<AppResponse<()>, CrossVmError> {
        let mut data = DISC_PING.to_vec();
        write_string(&mut data, &destination_port);
        let ix = self.svm_update_ix(wallet, data).await?;
        let meta = self
            .base
            .solana()?
            .send_transaction(vec![ix], WalletLabel::wrap(wallet))
            .await?;
        Ok(AppResponse::solana((), meta))
    }

    async fn svm_receive_packet(
        &self,
        wallet: &str,
        source_port: String,
        destination_port: String,
        sequence: u64,
        msg: String,
    ) -> Result<AppResponse<()>, CrossVmError> {
        let mut data = DISC_RECEIVE_PACKET.to_vec();
        write_string(&mut data, &source_port);
        write_string(&mut data, &destination_port);
        write_u64(&mut data, sequence);
        write_string(&mut data, &msg);
        let ix = self.svm_update_ix(wallet, data).await?;
        let meta = self
            .base
            .solana()?
            .send_transaction(vec![ix], WalletLabel::wrap(wallet))
            .await?;
        Ok(AppResponse::solana((), meta))
    }

    async fn svm_acknowledge_packet(
        &self,
        wallet: &str,
        source_port: String,
        destination_port: String,
        sequence: u64,
    ) -> Result<AppResponse<()>, CrossVmError> {
        let mut data = DISC_ACKNOWLEDGE_PACKET.to_vec();
        write_string(&mut data, &source_port);
        write_string(&mut data, &destination_port);
        write_u64(&mut data, sequence);
        let ix = self.svm_update_ix(wallet, data).await?;
        let meta = self
            .base
            .solana()?
            .send_transaction(vec![ix], WalletLabel::wrap(wallet))
            .await?;
        Ok(AppResponse::solana((), meta))
    }

    /// Read the `PingPongState` account: `[8 disc][chain_id: String][next_sequence: u64]
    /// [pings_sent: u64][pongs_received: u64][bump: u8]`.
    async fn svm_state(&self) -> Result<(String, StatsView), CrossVmError> {
        let pda = self.base.svm_addr()?;
        let acct = self
            .base
            .solana()?
            .get_account(&pda)
            .await?
            .ok_or_else(|| CrossVmError::Query {
                kind: ChainKind::Svm,
                reason: "ping-pong state account not found".into(),
            })?;
        let data = &acct.data;
        let mut o = 8; // skip the account discriminator
        let chain_id = read_string(data, &mut o);
        let next_sequence = read_u64(data, &mut o);
        let pings_sent = read_u64(data, &mut o);
        let pongs_received = read_u64(data, &mut o);
        Ok((
            chain_id,
            StatsView {
                pings_sent,
                pongs_received,
                next_sequence,
            },
        ))
    }

    async fn svm_stats(&self) -> Result<StatsView, CrossVmError> {
        Ok(self.svm_state().await?.1)
    }

    async fn svm_port(&self) -> Result<String, CrossVmError> {
        // The program emits its port from the program id (not the PDA): `{chain_id}.{program_id}`.
        let (chain_id, _) = self.svm_state().await?;
        Ok(format!("{chain_id}.{SOLANA_PROGRAM_ID}"))
    }
}
