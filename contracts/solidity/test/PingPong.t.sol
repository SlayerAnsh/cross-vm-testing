// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Test, Vm} from "forge-std/Test.sol";
import {PingPong} from "../src/PingPong.sol";

contract PingPongTest is Test {
    PingPong chainA;
    PingPong chainB;

    function setUp() public {
        chainA = new PingPong("1");
        chainB = new PingPong("42161");
    }

    function testInitialStatsAreZero() public view {
        assertEq(chainA.pingsSent(), 0);
        assertEq(chainA.pongsReceived(), 0);
        assertEq(chainA.nextSequence(), 0);
    }

    function testPingPongRoundTrip() public {
        string memory destB = chainB.selfPort();
        string memory sourceA = chainA.selfPort();

        vm.recordLogs();
        chainA.ping(destB);
        Vm.Log[] memory logs = vm.getRecordedLogs();
        assertEq(logs.length, 1);

        chainB.receivePacket(sourceA, destB, 0, "ping");

        chainA.acknowledgePacket(sourceA, destB, 0);

        assertEq(chainA.pingsSent(), 1);
        assertEq(chainA.nextSequence(), 1);
        assertEq(chainA.pongsReceived(), 1);
    }

    function testReceivePacketRejectsWrongDestination() public {
        string memory sourceA = chainA.selfPort();

        vm.expectRevert(bytes("invalid destination port"));
        chainB.receivePacket(sourceA, "wrong.port", 0, "ping");
    }

    function testReceivePacketRejectsWrongMessage() public {
        string memory sourceA = chainA.selfPort();
        string memory destB = chainB.selfPort();

        vm.expectRevert(bytes("invalid packet message"));
        chainB.receivePacket(sourceA, destB, 0, "not-ping");
    }
}
