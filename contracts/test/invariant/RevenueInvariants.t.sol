// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Test, console} from "forge-std/Test.sol";
import {StdInvariant} from "forge-std/StdInvariant.sol";
import {OnyxCommit} from "../../src/OnyxCommit.sol";
import {OnyxLedger} from "../../src/OnyxLedger.sol";
import {OnyxRevenue, IOnyxLedgerView, IRevenueToken} from "../../src/OnyxRevenue.sol";
import {MockUSDC3009} from "../mocks/MockUSDC3009.sol";

/// A ledger view with one shard per round id, each set by the handler (final or not, closed or
/// not, its reveals and its round's members with their Agree counts).
contract RevLedger is IOnyxLedgerView {
    struct S {
        bool fin;
        bool closed;
        bytes32 root;
        bytes32 binding;
        OnyxLedger.Reveal[] rv;
    }

    mapping(uint64 => mapping(bytes32 => S)) internal s;
    mapping(uint64 => address[]) internal members;
    mapping(uint64 => mapping(address => uint16)) internal agrees;

    function setShard(uint64 id, bytes32 shard, bool fin, bool closed, bytes32 root, bytes32 b)
        external
    {
        S storage x = s[id][shard];
        (x.fin, x.closed, x.root, x.binding) = (fin, closed, root, b);
    }

    function addReveal(uint64 id, bytes32 shard, address m, bytes32 r) external {
        s[id][shard].rv.push(OnyxLedger.Reveal({miner: m, root: r, binding: bytes32(0)}));
    }

    function addMember(uint64 id, address v, uint16 a) external {
        members[id].push(v);
        agrees[id][v] = a;
    }

    function isFinal(uint64 id, bytes32 shard) external view returns (bool) {
        return s[id][shard].fin;
    }

    function shardState(uint64 id, bytes32 shard)
        external
        view
        returns (OnyxLedger.ShardStatus, bool, uint64, bytes32, bytes32)
    {
        S storage x = s[id][shard];
        return (OnyxLedger.ShardStatus.Finalized, x.closed, id, x.root, x.binding);
    }

    function revealsOf(uint64 id, bytes32 shard)
        external
        view
        returns (OnyxLedger.Reveal[] memory)
    {
        return s[id][shard].rv;
    }

    function roundMembers(uint64 id) external view returns (address[] memory) {
        return members[id];
    }

    function tallyOf(uint64 id, bytes32, address v)
        external
        view
        returns (OnyxLedger.Tally memory t)
    {
        t.agrees = agrees[id][v];
    }
}

/// Handler for the OnyxRevenue invariants (L2 contract audit): shards registered from the
/// ledger, fees by `pay`, by self-settled x402 (`settle`) and by facilitator-settled receipts
/// (`credit`, total-capped), donations nobody can attribute, claims, withdrawals, label
/// payees, reporter rotation, and non-role calls that must revert.
contract RevenueHandler is Test {
    MockUSDC3009 public usdc;
    RevLedger public ledger;
    OnyxRevenue public rev;

    address public constant TREASURY = address(0x7EA5);
    address[2] public reporters = [address(0x4E01), address(0x4E02)];
    address[] public miners;
    address[] public validators;
    address[] public payers;
    address[] public payees;
    /// Every address `rev.balanceOf` can credit.
    address[] public actors;
    bytes32[3] public labelSets = [bytes32(0), keccak256("labels A"), keccak256("labels B")];

    struct Book {
        uint64 id;
        bytes32 shard;
        bool shardBook;
    }

    struct Pending {
        address payer;
        bytes32 nonce;
        uint128 amount;
    }

    /// Shards set in the ledger (registered or not), then books opened.
    Book[] public shards;
    Book[] public books;
    Pending[] internal pending;
    mapping(uint64 => mapping(bytes32 => OnyxCommit.CommitmentFields)) internal fieldsOf;
    mapping(uint64 => mapping(bytes32 => uint256)) public creditedTo;

    uint256 public ghostIn;
    uint256 public ghostOut;
    uint256 public breaches;
    string public lastBreach;
    uint256 internal nonceCtr;
    mapping(bytes32 => uint256) public ok;

    constructor() {
        usdc = new MockUSDC3009();
        ledger = new RevLedger();
        // An uneven split so per-head rounding is exercised: miners, validators, labels, treasury.
        rev = new OnyxRevenue(
            IRevenueToken(address(usdc)),
            IOnyxLedgerView(address(ledger)),
            reporters[0],
            TREASURY,
            [uint16(4_999), 2_001, 1_003, 1_997]
        );
        for (uint256 i = 0; i < 4; i++) {
            miners.push(address(uint160(0x3100 + i)));
            validators.push(address(uint160(0x3200 + i)));
            actors.push(miners[i]);
            actors.push(validators[i]);
        }
        for (uint256 i = 0; i < 3; i++) {
            payers.push(address(uint160(0x3300 + i)));
            actors.push(payers[i]);
        }
        payees.push(address(0x3401));
        payees.push(address(0x3402));
        actors.push(payees[0]);
        actors.push(payees[1]);
        actors.push(TREASURY);
        actors.push(reporters[0]);
        actors.push(reporters[1]);
        vm.warp(1_000);
    }

    modifier tracked(int8 dir) {
        uint256 before = usdc.balanceOf(address(rev));
        _;
        uint256 a = usdc.balanceOf(address(rev));
        if (a > before) {
            if (dir != 1) _breach("tokens in on a non-payment path");
            ghostIn += a - before;
        } else if (a < before) {
            if (dir != -1) _breach("tokens out on a non-withdraw path");
            ghostOut += before - a;
        }
    }

    function _breach(string memory why) internal {
        breaches++;
        lastBreach = why;
    }

    function _book(uint256 seed) internal view returns (Book storage) {
        return books[seed % books.length];
    }

    // ------------------------------------------------------------------ ledger side

    /// A new shard in the ledger: final (90%), closed by a case or not, reveals of the final
    /// root or another, members with or without an Agree, a label set or none.
    function newShard(uint256 seed) external tracked(0) {
        if (shards.length >= 16) return;
        uint64 id = uint64(shards.length + 1);
        bytes32 shard = keccak256(abi.encode("shard", id));
        bytes32 root = keccak256(abi.encode("root", id));
        OnyxCommit.CommitmentFields memory f;
        f.root = root;
        f.leafCount = uint64(seed % 1000);
        f.recipeId = keccak256("recipe");
        f.labelSetHash = labelSets[(seed >> 8) % 3];
        fieldsOf[id][shard] = f;
        ledger.setShard(
            id,
            shard,
            (seed >> 16) % 10 != 0,
            (seed >> 20) % 3 == 0,
            root,
            OnyxCommit.bindingHash(f)
        );
        for (uint256 i = 0; i < 4; i++) {
            uint256 x = uint256(keccak256(abi.encode(seed, "m", i)));
            if (x % 4 == 0) continue;
            ledger.addReveal(id, shard, miners[i], x % 4 == 1 ? keccak256("other") : root);
            // A miner may reveal twice in the view: register counts it once.
            if (x % 16 == 3) ledger.addReveal(id, shard, miners[i], root);
        }
        for (uint256 i = 0; i < 4; i++) {
            uint256 x = uint256(keccak256(abi.encode(seed, "v", i)));
            if (x % 3 == 0) continue;
            ledger.addMember(id, validators[i], uint16(x % 3 == 1 ? 1 : 0));
        }
        shards.push(Book({id: id, shard: shard, shardBook: true}));
    }

    function register(uint256 sSeed, bool corrupt, address caller) external tracked(0) {
        if (shards.length == 0) return;
        Book storage b = shards[sSeed % shards.length];
        OnyxCommit.CommitmentFields memory f = fieldsOf[b.id][b.shard];
        if (corrupt) f.labelSetHash = keccak256("a set the shard never pinned");
        bool fin = ledger.isFinal(b.id, b.shard);
        vm.prank(caller);
        try rev.register(b.id, b.shard, f) {
            ok["register"]++;
            if (corrupt) _breach("registered with fields that do not hash to the final binding");
            if (!fin) _breach("registered a shard that is not final");
            books.push(b);
        } catch {}
    }

    function registerObservation(uint256 seed) external tracked(0) {
        bytes32 fact = keccak256(abi.encode("fact", seed % 4));
        vm.prank(rev.reporter());
        try rev.registerObservation(fact) {
            ok["registerObservation"]++;
            books.push(Book({id: rev.OBSERVATION_ROUND(), shard: fact, shardBook: false}));
        } catch {}
    }

    // ------------------------------------------------------------------ fees

    /// Owed by `rev` in total: what `allocated - paidOut` must equal.
    function owedTotal() public view returns (uint256 t) {
        for (uint256 i = 0; i < actors.length; i++) {
            t += rev.balanceOf(actors[i]);
        }
        for (uint256 i = 0; i < 3; i++) {
            t += rev.labelHeld(labelSets[i]);
        }
        for (uint256 i = 0; i < books.length; i++) {
            t += unclaimed(books[i]);
        }
    }

    function unclaimed(Book memory b) public view returns (uint256 t) {
        (,,,,, uint128 pm, uint128 pv,) = rev.books(b.id, b.shard);
        for (uint256 j = 0; j < 4; j++) {
            if (rev.roleOf(b.id, b.shard, miners[j]) & 1 != 0) {
                t += pm - rev.claimedMiner(b.id, b.shard, miners[j]);
            }
            if (rev.roleOf(b.id, b.shard, validators[j]) & 2 != 0) {
                t += pv - rev.claimedValidator(b.id, b.shard, validators[j]);
            }
            // A validator that is also a miner of the shard cannot happen here (disjoint sets).
        }
    }

    /// Every fee is split with nothing lost or created: owed rises by exactly the fee.
    function _checkSplit(uint256 owedBefore, uint256 allocBefore, uint256 amount) internal {
        if (rev.allocated() - allocBefore != amount) {
            _breach("allocated moved by other than the fee");
        }
        if (owedTotal() - owedBefore != amount) _breach("a fee split created or lost money");
    }

    function pay(uint256 bSeed, uint256 pSeed, uint128 amount) external tracked(1) {
        if (books.length == 0) return;
        Book storage b = _book(bSeed);
        address p = payers[pSeed % payers.length];
        uint128 a = amount % 1_000_000_000;
        usdc.mint(p, a);
        vm.prank(p);
        usdc.approve(address(rev), a);
        (uint256 o, uint256 al) = (owedTotal(), rev.allocated());
        vm.prank(p);
        try rev.pay(b.id, b.shard, a) {
            ok["pay"]++;
            _checkSplit(o, al, a);
            creditedTo[b.id][b.shard] += a;
        } catch {}
    }

    /// The reporter settles a client's x402 authorization here itself (ADR-0026).
    function settle(uint256 bSeed, uint256 pSeed, uint128 amount, bool reuse) external tracked(1) {
        if (books.length == 0) return;
        Book storage b = _book(bSeed);
        address p = payers[pSeed % payers.length];
        uint128 a = amount % 1_000_000_000;
        bytes32 nonce = reuse && pending.length > 0
            ? pending[amount % pending.length].nonce
            : keccak256(abi.encode("nonce", nonceCtr++));
        usdc.mint(p, a);
        OnyxRevenue.Authorization memory au = OnyxRevenue.Authorization({
            from: p,
            value: a,
            validAfter: block.timestamp - 1,
            validBefore: block.timestamp + 100,
            nonce: nonce
        });
        (uint256 o, uint256 al) = (owedTotal(), rev.allocated());
        vm.prank(rev.reporter());
        try rev.settle(b.id, b.shard, au, "") {
            ok["settle"]++;
            _checkSplit(o, al, a);
            creditedTo[b.id][b.shard] += a;
        } catch {}
    }

    /// A facilitator settles an x402 payment to `rev` (funds arrive unattributed).
    function facilitatorSettle(uint256 pSeed, uint128 amount) external tracked(1) {
        if (pending.length >= 32) return;
        address p = payers[pSeed % payers.length];
        uint128 a = amount % 1_000_000_000;
        bytes32 nonce = keccak256(abi.encode("nonce", nonceCtr++));
        usdc.mint(p, a);
        usdc.transferWithAuthorization(p, address(rev), a, nonce);
        pending.push(Pending({payer: p, nonce: nonce, amount: a}));
        ok["facilitator"]++;
    }

    /// Tokens sent with a plain transfer: no authorization, so `credit` can never attribute them.
    function donate(uint128 amount) external tracked(1) {
        uint128 a = amount % 1_000_000;
        usdc.mint(address(this), a);
        usdc.transfer(address(rev), a);
    }

    /// The reporter attributes facilitator payments: some honest, some inflated, reused or never
    /// paid. Whatever applies must be capped by what arrived and was not yet attributed.
    function credit(uint256 seed, uint8 n) external tracked(0) {
        if (books.length == 0) return;
        uint256 k = 1 + n % 4;
        OnyxRevenue.Receipt[] memory rs = new OnyxRevenue.Receipt[](k);
        for (uint256 i = 0; i < k; i++) {
            uint256 x = uint256(keccak256(abi.encode(seed, i)));
            Book storage b = books[x % books.length];
            Pending memory p = pending.length > 0
                ? pending[(x >> 8) % pending.length]
                : Pending({payer: payers[0], nonce: keccak256("never"), amount: 1});
            uint128 amount = p.amount;
            if ((x >> 16) % 4 == 0) amount = amount * 2 + 1;
            if ((x >> 20) % 5 == 0) p.nonce = keccak256(abi.encode("unused", x));
            rs[i] = OnyxRevenue.Receipt({
                round: b.id, shard: b.shard, amount: amount, payer: p.payer, nonce: p.nonce
            });
        }
        uint256 room = rev.unallocated();
        // Independent model of which receipts apply: unused (on chain and earlier in this
        // batch), reported used by the token, and within what is still unallocated.
        bool[] memory applies = new bool[](k);
        uint256 left = room;
        uint256 expectApplied;
        for (uint256 i = 0; i < k; i++) {
            bytes32 key = keccak256(abi.encode(rs[i].payer, rs[i].nonce));
            bool used = rev.receiptUsed(key);
            for (uint256 j = 0; j < i; j++) {
                if (applies[j] && keccak256(abi.encode(rs[j].payer, rs[j].nonce)) == key) {
                    used = true;
                }
            }
            if (!used && usdc.authorizationState(rs[i].payer, rs[i].nonce) && rs[i].amount <= left)
            {
                applies[i] = true;
                left -= rs[i].amount;
                expectApplied++;
            }
        }
        (uint256 o, uint256 al) = (owedTotal(), rev.allocated());
        vm.prank(rev.reporter());
        try rev.credit(rs) returns (uint256 applied) {
            ok["credit"] += applied;
            uint256 added = rev.allocated() - al;
            if (applied != expectApplied || added != room - left) {
                _breach("credit applied other receipts than the model");
            }
            if (added > room) _breach("credit attributed more than arrived unallocated");
            if (owedTotal() - o != added) _breach("credit split created or lost money");
            for (uint256 i = 0; i < k; i++) {
                if (applies[i]) creditedTo[rs[i].round][rs[i].shard] += rs[i].amount;
            }
        } catch {}
    }

    // ------------------------------------------------------------------ payouts and roles

    function claim(uint256 bSeed, uint256 wSeed) external tracked(0) {
        if (books.length == 0) return;
        Book storage b = _book(bSeed);
        address w = actors[wSeed % actors.length];
        uint256 o = owedTotal();
        try rev.claim(b.id, b.shard, w) {
            ok["claim"]++;
            if (owedTotal() != o) _breach("claim changed what is owed");
        } catch {}
    }

    function withdraw(uint256 wSeed) external tracked(-1) {
        address w = actors[wSeed % actors.length];
        uint256 owed = rev.balanceOf(w);
        uint256 before = usdc.balanceOf(w);
        vm.prank(w);
        try rev.withdraw() {
            ok["withdraw"]++;
            if (usdc.balanceOf(w) - before != owed) {
                _breach("withdraw paid other than the balance");
            }
        } catch {}
    }

    function setLabelPayee(uint256 lSeed, uint256 pSeed) external tracked(0) {
        bytes32 ls = labelSets[1 + lSeed % 2];
        uint256 k = pSeed % 3;
        address p = k == 2 ? address(0) : payees[k];
        uint256 o = owedTotal();
        vm.prank(rev.reporter());
        try rev.setLabelPayee(ls, p) {
            ok["setLabelPayee"]++;
            if (owedTotal() != o) _breach("naming a payee changed what is owed");
        } catch {}
    }

    function setReporter(uint256 rSeed) external tracked(0) {
        vm.prank(TREASURY);
        try rev.setReporter(reporters[rSeed % 2]) {
            ok["setReporter"]++;
        } catch {}
    }

    function warp(uint16 secs) external tracked(0) {
        vm.warp(block.timestamp + 1 + secs % 300);
        vm.roll(block.number + 1);
    }

    /// Non-role accounts: every privileged call must revert.
    function attack(uint8 which, uint256 aSeed) external tracked(0) {
        address a = actors[aSeed % actors.length];
        if (a == rev.reporter() || a == TREASURY) a = address(0xBAD);
        bool success;
        uint256 w = which % 5;
        if (w == 0) {
            OnyxRevenue.Receipt[] memory rs = new OnyxRevenue.Receipt[](0);
            vm.prank(a);
            try rev.credit(rs) {
                success = true;
            } catch {}
        } else if (w == 1) {
            vm.prank(a);
            try rev.setLabelPayee(labelSets[1], a) {
                success = true;
            } catch {}
        } else if (w == 2) {
            vm.prank(a);
            try rev.registerObservation(keccak256(abi.encode("attack", aSeed))) {
                success = true;
            } catch {}
        } else if (w == 3) {
            vm.prank(a);
            try rev.setReporter(a) {
                success = true;
            } catch {}
        } else {
            OnyxRevenue.Authorization memory au;
            uint64 id = books.length > 0 ? books[0].id : 1;
            bytes32 s = books.length > 0 ? books[0].shard : bytes32(0);
            vm.prank(a);
            try rev.settle(id, s, au, "") {
                success = true;
            } catch {}
        }
        if (success) _breach("a privileged revenue call succeeded from a non-role account");
    }

    // ------------------------------------------------------------------ full payout

    /// Claim every role on every book, name a payee for every held label share, withdraw all.
    function finishAll() external {
        for (uint256 i = 0; i < books.length; i++) {
            for (uint256 j = 0; j < 4; j++) {
                rev.claim(books[i].id, books[i].shard, miners[j]);
                rev.claim(books[i].id, books[i].shard, validators[j]);
            }
        }
        vm.startPrank(rev.reporter());
        for (uint256 i = 1; i < 3; i++) {
            if (rev.labelHeld(labelSets[i]) > 0) rev.setLabelPayee(labelSets[i], payees[0]);
        }
        vm.stopPrank();
        uint256 before = usdc.balanceOf(address(rev));
        for (uint256 i = 0; i < actors.length; i++) {
            vm.prank(actors[i]);
            rev.withdraw();
        }
        ghostOut += before - usdc.balanceOf(address(rev));
    }

    function booksLength() external view returns (uint256) {
        return books.length;
    }

    function bookAt(uint256 i) external view returns (Book memory) {
        return books[i];
    }
}

/// OnyxRevenue money invariants under random sequences (L2 contract audit).
contract RevenueInvariantTest is Test {
    RevenueHandler internal h;

    function setUp() public {
        h = new RevenueHandler();
        targetContract(address(h));
        bytes4[] memory s = new bytes4[](15);
        s[0] = h.newShard.selector;
        s[1] = h.register.selector;
        s[2] = h.registerObservation.selector;
        s[3] = h.pay.selector;
        s[4] = h.settle.selector;
        s[5] = h.facilitatorSettle.selector;
        s[6] = h.donate.selector;
        s[7] = h.credit.selector;
        s[8] = h.claim.selector;
        s[9] = h.withdraw.selector;
        s[10] = h.setLabelPayee.selector;
        s[11] = h.setReporter.selector;
        s[12] = h.warp.selector;
        s[13] = h.attack.selector;
        s[14] = h.newShard.selector; // twice: shards feed every other action
        targetSelector(StdInvariant.FuzzSelector({addr: address(h), selectors: s}));
    }

    /// Everything ever allocated is either paid out or still owed to someone: a balance, a held
    /// label share, or an unclaimed per-head share of a book. Exact.
    function invariant_AllocatedIsPaidOrOwed() public view {
        OnyxRevenue rev = h.rev();
        assertEq(rev.allocated(), rev.paidOut() + h.owedTotal(), "allocated != paid + owed");
    }

    /// Nothing is allocated that did not arrive; the token balance is exactly inflows less
    /// withdrawals, and withdrawals are `paidOut`.
    function invariant_NeverAllocatesMoreThanArrived() public view {
        OnyxRevenue rev = h.rev();
        uint256 bal = h.usdc().balanceOf(address(rev));
        assertGe(bal + rev.paidOut(), rev.allocated(), "allocated more than arrived");
        assertEq(bal, h.ghostIn() - h.ghostOut(), "flows");
        assertEq(rev.paidOut(), h.ghostOut(), "paidOut");
        assertEq(rev.unallocated(), bal + rev.paidOut() - rev.allocated(), "unallocated");
    }

    /// Each book's heads never hold more than the book earned, and the book's revenue is
    /// exactly what was credited to it.
    function invariant_BooksMatchTheirFees() public view {
        OnyxRevenue rev = h.rev();
        for (uint256 i = 0; i < h.booksLength(); i++) {
            RevenueHandler.Book memory b = h.bookAt(i);
            (, uint32 m, uint32 v,,, uint128 pm, uint128 pv, uint128 revenue) =
                rev.books(b.id, b.shard);
            assertLe(uint256(pm) * m + uint256(pv) * v, revenue, "heads > revenue");
            assertEq(revenue, h.creditedTo(b.id, b.shard) + 0, "book revenue");
        }
    }

    function invariant_NoBreaches() public view {
        assertEq(h.breaches(), 0, h.lastBreach());
        assertEq(h.rev().owner(), h.TREASURY(), "owner");
    }

    /// No stuck funds: after every role claims and withdraws and held label shares get a payee,
    /// the contract holds exactly what arrived but was never attributed.
    function afterInvariant() public {
        h.finishAll();
        OnyxRevenue rev = h.rev();
        assertEq(rev.allocated(), rev.paidOut(), "owed after full payout");
        assertEq(h.owedTotal(), 0, "owed left");
        assertEq(
            h.usdc().balanceOf(address(rev)), rev.unallocated(), "only unattributed funds remain"
        );
        _stats(
            string.concat(
                "STATS revenue books=",
                vm.toString(h.booksLength()),
                " pays=",
                vm.toString(h.ok("pay")),
                " settles=",
                vm.toString(h.ok("settle")),
                " credited=",
                vm.toString(h.ok("credit")),
                " claims=",
                vm.toString(h.ok("claim")),
                " withdrawals=",
                vm.toString(h.ok("withdraw"))
            )
        );
    }

    /// One line per run: printed, and appended to cache/invariant-stats/ when
    /// ONYX_INVARIANT_STATS=1 (the audit run sums them; docs/audit/2026-10-13-contracts.md).
    function _stats(string memory a) internal {
        console.log(a);
        if (vm.envOr("ONYX_INVARIANT_STATS", false)) {
            vm.writeLine("cache/invariant-stats/revenue.log", a);
        }
    }
}
