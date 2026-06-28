//! Integration test: deploy -> execute -> query a Solidity `Counter` through the EVM
//! provider. ABI encoding/decoding uses alloy's `sol!`; the creation bytecode below was
//! produced by `forge build` (Solc 0.8.33) for:
//!
//! ```solidity
//! contract Counter {
//!     uint256 public number;
//!     function setNumber(uint256 n) public { number = n; }
//!     function increment() public { number += 1; }
//! }
//! ```

use alloy::sol;
use alloy::sol_types::SolCall;
use cross_vm_core::ChainProvider;
use cross_vm_solidity::chains::LOCAL;
use revm::primitives::{Bytes, U256};

sol! {
    contract Counter {
        function setNumber(uint256 n) external;
        function increment() external;
        function number() external view returns (uint256);
    }
}

const COUNTER_CREATION_BYTECODE: &str = "6080604052348015600e575f5ffd5b506101cf8061001c5f395ff3fe608060405234801561000f575f5ffd5b506004361061003f575f3560e01c80633fb5c1cb146100435780638381f58a1461005f578063d09de08a1461007d575b5f5ffd5b61005d600480360381019061005891906100e6565b610087565b005b610067610090565b6040516100749190610120565b60405180910390f35b610085610095565b005b805f8190555050565b5f5481565b60015f5f8282546100a69190610166565b92505081905550565b5f5ffd5b5f819050919050565b6100c5816100b3565b81146100cf575f5ffd5b50565b5f813590506100e0816100bc565b92915050565b5f602082840312156100fb576100fa6100af565b5b5f610108848285016100d2565b91505092915050565b61011a816100b3565b82525050565b5f6020820190506101335f830184610111565b92915050565b7f4e487b71000000000000000000000000000000000000000000000000000000005f52601160045260245ffd5b5f610170826100b3565b915061017b836100b3565b925082820190508082111561019357610192610139565b5b9291505056fea26469706673582212206d6ef14bff0baa06c826194d45bd4f624ad75acdf82768b1a1a826acf5acefbd64736f6c63430008210033";

fn read_number(chain: &cross_vm_solidity::EvmMockProvider, contract: &revm::primitives::Address) -> U256 {
    let out = chain
        .query(contract, Bytes::from(Counter::numberCall {}.abi_encode()))
        .expect("query number");
    Counter::numberCall::abi_decode_returns(&out).expect("decode number")
}

#[test]
fn deploy_set_increment_query() {
    let mut chain = LOCAL.mock();
    let deployer = chain.new_account("deployer");

    let code = Bytes::from(hex::decode(COUNTER_CREATION_BYTECODE).unwrap());
    let contract = chain
        .deploy(code, Bytes::new(), &deployer)
        .expect("deploy");

    // Fresh counter starts at zero.
    assert_eq!(read_number(&chain, &contract), U256::ZERO);

    // setNumber(41)
    chain
        .execute(
            &contract,
            Bytes::from(
                Counter::setNumberCall {
                    n: U256::from(41u64),
                }
                .abi_encode(),
            ),
            &deployer,
        )
        .expect("setNumber");
    assert_eq!(read_number(&chain, &contract), U256::from(41u64));

    // increment()
    chain
        .execute(
            &contract,
            Bytes::from(Counter::incrementCall {}.abi_encode()),
            &deployer,
        )
        .expect("increment");
    assert_eq!(read_number(&chain, &contract), U256::from(42u64));
}
