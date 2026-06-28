// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

contract PingPong {
    bytes internal constant PING_MSG = "ping";
    bytes internal constant PONG_ACK = "pong";

    string public chainId;
    uint64 public nextSequence;
    uint64 public pingsSent;
    uint64 public pongsReceived;

    event SendPacket(
        string source_port,
        string destination_port,
        uint64 packet_sequence,
        bytes msg
    );
    event ReceivePacket(
        string source_port,
        string destination_port,
        uint64 packet_sequence
    );
    event WriteAcknowledgement(
        string source_port,
        string destination_port,
        uint64 packet_sequence,
        bytes msg,
        bytes ack
    );
    event AcknowledgePacket(
        string source_port,
        string destination_port,
        uint64 packet_sequence
    );

    constructor(string memory _chainId) {
        chainId = _chainId;
    }

    function ping(string calldata destinationPort) external {
        string memory srcPort = _sourcePort();
        uint64 sequence = nextSequence;

        pingsSent += 1;
        nextSequence += 1;

        emit SendPacket(srcPort, destinationPort, sequence, PING_MSG);
    }

    function receivePacket(
        string calldata sourcePort,
        string calldata destinationPort,
        uint64 sequence,
        bytes calldata packetMsg
    ) external {
        require(
            keccak256(bytes(destinationPort)) == keccak256(bytes(_sourcePort())),
            "invalid destination port"
        );
        require(keccak256(packetMsg) == keccak256(PING_MSG), "invalid packet message");

        emit ReceivePacket(sourcePort, destinationPort, sequence);
        emit WriteAcknowledgement(
            sourcePort,
            destinationPort,
            sequence,
            packetMsg,
            PONG_ACK
        );
    }

    function acknowledgePacket(
        string calldata sourcePort,
        string calldata destinationPort,
        uint64 sequence
    ) external {
        pongsReceived += 1;
        emit AcknowledgePacket(sourcePort, destinationPort, sequence);
    }

    function selfPort() public view returns (string memory) {
        return _sourcePort();
    }

    function _sourcePort() internal view returns (string memory) {
        return string(abi.encodePacked(chainId, ".", _addressToString(address(this))));
    }

    function _addressToString(address addr) internal pure returns (string memory) {
        bytes memory data = abi.encodePacked(addr);
        bytes memory alphabet = "0123456789abcdef";
        bytes memory str = new bytes(2 + data.length * 2);
        str[0] = "0";
        str[1] = "x";
        for (uint256 i = 0; i < data.length; i++) {
            str[2 + i * 2] = alphabet[uint8(data[i] >> 4)];
            str[3 + i * 2] = alphabet[uint8(data[i] & 0x0f)];
        }
        return string(str);
    }
}
