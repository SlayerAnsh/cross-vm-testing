// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {Counter} from "../src/Counter.sol";

contract CounterTest is Test {
    Counter counter;

    function setUp() public {
        counter = new Counter();
    }

    function testInitialCountIsZero() public view {
        assertEq(counter.count(), 0);
    }

    function testIncrementTwice() public {
        counter.increment();
        counter.increment();
        assertEq(counter.count(), 2);
    }

    function testReset() public {
        counter.increment();
        counter.increment();
        counter.reset();
        assertEq(counter.count(), 0);
    }
}
