//! TVM-specific opcodes absent from stock `revm`.
//!
//! tronc (TRON's `solc` fork) emits TRON-native opcodes that the EVM instruction set lacks. Even a
//! trivial non-payable function carries a TRC-10 guard prelude (`CALLTOKENVALUE; ISZERO; JUMPI`
//! and `CALLTOKENID; ISZERO; JUMPI`) that reverts if any TRC-10 token was attached to the call, so
//! `revm` halts with `OpcodeNotFound` on tronc-compiled bytecode. The mock injects minimal
//! implementations of these opcodes so that bytecode runs.
//!
//! The mock has no TRC-10 token ledger, so the token opcodes report "no token attached" (value and
//! id 0, balance 0). That is faithful for contracts that do not use TRC-10 (the only ones the mock
//! is meant to run) and lets tronc bytecode execute. `CALLTOKEN` (0xd0) is a CALL variant that
//! transfers a TRC-10 token and needs a ledger the mock does not model, so it is left unimplemented
//! (bytecode that reaches it still halts).
//!
//! Source: java-tron `org.tron.core.vm.OpCode`.

use alloy_primitives::U256;
use revm::context_interface::Host;
use revm::interpreter::interpreter_types::StackTr;
use revm::interpreter::{InstructionContext, InstructionResult, InterpreterTypes};

/// `CALLTOKEN`: CALL variant transferring a TRC-10 token. Not implemented (no token ledger).
pub const CALLTOKEN: u8 = 0xd0;
/// `TOKENBALANCE`: TRC-10 token balance of an address.
pub const TOKENBALANCE: u8 = 0xd1;
/// `CALLTOKENVALUE`: TRC-10 token value sent with the current call.
pub const CALLTOKENVALUE: u8 = 0xd2;
/// `CALLTOKENID`: TRC-10 token id sent with the current call.
pub const CALLTOKENID: u8 = 0xd3;
/// `ISCONTRACT`: whether an address holds deployed code.
pub const ISCONTRACT: u8 = 0xd4;

/// Base gas charged for each injected opcode. The mock's energy/bandwidth accounting is a coarse
/// shim ([`super::resources`]), so the exact cost is not significant; this matches `revm`'s
/// cheapest tier.
pub const TVM_OPCODE_GAS: u16 = 2;

/// `CALLTOKENVALUE` (0xd2): the TRC-10 token value sent with the call. The mock attaches none, so 0.
pub fn call_token_value<W: InterpreterTypes, H: ?Sized>(
    ctx: InstructionContext<'_, H, W>,
) -> Result<(), InstructionResult> {
    push(ctx, U256::ZERO)
}

/// `CALLTOKENID` (0xd3): the TRC-10 token id sent with the call. None attached, so 0.
pub fn call_token_id<W: InterpreterTypes, H: ?Sized>(
    ctx: InstructionContext<'_, H, W>,
) -> Result<(), InstructionResult> {
    push(ctx, U256::ZERO)
}

/// `TOKENBALANCE` (0xd1): TRC-10 `tokenId` balance of `address`. Pops `[tokenId, address]`; the mock
/// holds no TRC-10 balances, so the result is 0.
pub fn token_balance<W: InterpreterTypes, H: ?Sized>(
    ctx: InstructionContext<'_, H, W>,
) -> Result<(), InstructionResult> {
    if ctx.interpreter.stack.popn::<2>().is_none() {
        return Err(InstructionResult::StackUnderflow);
    }
    push(ctx, U256::ZERO)
}

/// `ISCONTRACT` (0xd4): 1 if `address` holds deployed code, else 0. Pops `[address]`.
pub fn is_contract<W: InterpreterTypes, H: Host + ?Sized>(
    ctx: InstructionContext<'_, H, W>,
) -> Result<(), InstructionResult> {
    let Some(address) = ctx.interpreter.stack.pop_address() else {
        return Err(InstructionResult::StackUnderflow);
    };
    let has_code = ctx
        .host
        .load_account_code(address)
        .map(|code| !code.data.is_empty())
        .unwrap_or(false);
    push(ctx, U256::from(has_code as u8))
}

/// Push `value`, mapping a stack overflow to the matching halt.
fn push<W: InterpreterTypes, H: ?Sized>(
    ctx: InstructionContext<'_, H, W>,
    value: U256,
) -> Result<(), InstructionResult> {
    if ctx.interpreter.stack.push(value) {
        Ok(())
    } else {
        Err(InstructionResult::StackOverflow)
    }
}
