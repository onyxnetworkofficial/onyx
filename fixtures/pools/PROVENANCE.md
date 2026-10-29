# Pool list provenance (ADR-0032)

| Field | Value |
|---|---|
| Committed file | `pool-allowlist.json` (24,728 bytes, 127 reviewed pools; SHA-256 `57367da78c0f771786210fc24bc6e16b6478ca7a87a496fa37ce807a47db031a` in `../SHA256SUMS`) |
| Upstream | https://github.com/bitcoin-data/mining-pools, branch `generated` (https://github.com/bitcoin-data/mining-pools/tree/generated), file `pool-list.json`: 31,736 bytes, 149 pools, SHA-256 `ddc505c82dfa5868ce234be3ac0c05454bfbd6e1d34fd1e37f3004cde0a5ef0e` |
| How it got here | `scripts/build-pool-allowlist.py <upstream pool-list.json>`, run on the the entity indexer's byte-identical copy (`fixtures/groundtruth/pool-list.json`, indexer commit 9520235, 2026-08-10). The script refuses any other upstream bytes and writes the same output every time. |
| Why the upstream file is not committed | Some upstream entries are people's names or handles (solo miners). Onyx never publishes those (CLAUDE.md hard limit; review POOL-R1). |
| License | MIT. The full notice (copyright btc.com 2019-2021, 0xB10C and contributors 2021-) ships in `LICENSE-mining-pools.txt`, as MIT requires for copies and substantial portions. |

## Decisions, one per upstream id (POOL-R1-F4)
Rule: named only with a company website (http(s); not a github.com issue, a forum post, a bare
IP address or an explorer page), not run under an operator's personal handle, not a person's
name, and with at least one non-empty tag or address. Excluded ids are listed by id and reason
class only, never by name.

### Excluded (22)
| Upstream id | Reason |
|---|---|
| 13 | no company site |
| 17 | no company site |
| 21 | person |
| 23 | no company site |
| 30 | person |
| 40 | person |
| 52 | person |
| 53 | person |
| 65 | no company site |
| 66 | operator handle |
| 69 | no company site |
| 75 | operator handle |
| 89 | no company site |
| 101 | no company site |
| 109 | no company site |
| 112 | no company site |
| 115 | operator handle (site on an operator-handle domain; POOL-R2-F6) |
| 118 | operator handle |
| 131 | no company site |
| 147 | no company site |
| 148 | no company site |
| 154 | no company site |

### Named (127)
1 Bitcoin Affiliate Network, 2 8baochi, 3 EMCDPool, 4 Luxor, 5 OKMINER, 6 Bitcoin.com, 7 BytePool, 8 Minerium, 9 CANOE, 10 TripleMining, 11 NovaBlock, 12 PHash.IO, 14 UKRPool, 15 simplecoin.us, 16 Give Me Coins, 18 TogetherPool, 19 Cointerra, 20 OzCoin, 22 F2Pool, 24 Bitdeer, 25 OKKONG, 26 GHash.IO, 27 RigPool, 28 NMCbit, 29 MultiCoin.co, 31 SpiderPool, 32 OKExPool, 33 Eligius, 34 175btc, 35 Bravo Mining, 36 DCEX, 37 Bitcoin-Ukraine, 38 HashBX, 39 MegaBigPower, 41 BATPOOL, 42 Yourbtc.net, 43 BTC.com, 44 transactioncoinmining, 45 BitClub, 46 myBTCcoin Pool, 47 BTC Guild, 48 Sigmapool.com, 49 BTC.TOP, 50 Waterhole, 51 DPOOL, 54 BitFury, 55 TATMAS Pool, 56 mmpool, 57 Polmine, 58 HASHPOOL, 59 Multipool, 60 Tangpool, 61 AntPool, 62 1Hash, 63 BTCDig, 64 DCExploration, 68 Nexious, 70 1THash, 71 KuCoin Pool, 72 Ultimus Pool, 73 BTCServ, 74 Pega Pool, 76 Terra Pool, 77 Mt Red, 78 MiningCity, 79 Bitparking, 80 KnCMiner, 81 Telco 214, 82 A-XBT, 83 EclipseMC, 84 Titan, 85 BitMinter, 86 GoGreenLight, 87 haominer, 88 Foundry USA, 90 ConnectBTC, 91 Bitfarms, 92 Eobot, 93 Hummerpool, 94 HAOZHUZHU, 95 BitcoinIndia, 96 Lubian.com, 97 7pool, 98 TMSPool, 99 okpool.top, 100 BWPool, 102 digitalX Mintsy, 103 NiceHash, 104 BitcoinRussia, 105 50BTC, 106 SBI Crypto, 107 BTCC, 108 MaxBTC, 110 ViaBTC, 111 Poolin, 113 TBDice, 114 CanoePool, 116 21 Inc., 117 BlockFills, 119 Braiins Pool, 120 58COIN, 121 HotPool, 122 Bitalo, 123 Binance Pool, 124 WAYI.CN, 125 BCMonster, 126 Huobi Pool, 127 AAO Pool, 128 Bixin, 129 CoinLab, 130 GBMiners, 132 ASICMiner, 133 Bitsolo, 134 digitalBTC, 135 CloudHashing, 136 Rawpool, 137 EXX&BW, 138 BTCMP, 139 bcpool.io, 140 MARA Pool, 145 Ocean.xyz, 146 SecPool, 149 BitFuFu, 150 MaxiPool, 151 Mining-Dutch, 152 Mining Squared, 153 Innopolis Tech.

A list update is a new upstream file: pin its SHA-256 in the script, review every new or changed
entry by the rule above, regenerate the allowlist, and expect a new table hash, new shard ids and
new site pins.
