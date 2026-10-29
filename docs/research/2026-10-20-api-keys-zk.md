# Research: letting Onyx pull keyed API data without seeing the key (ROADMAP S2, 2026-10-20)

From a research subagent. Every claim about another project carries a link that was opened on
2026-10-20. "(Onyx proposal)" marks our own suggestions. Pages that could not be fetched are
named as such.

## Question
Can a user let Onyx read data from an API that needs their key (for example a usage or billing
API), so that Onyx gets the data without ever seeing the key, plus a proof that the data came
from that API?

## Short answer
- Yes for "without seeing the key", but not "Onyx pulls it alone". In every design below the
  user's own software holds the key and runs the fetch; a second party helps run the TLS session.
- The proof says "this server, under this TLS certificate name, sent these bytes". It is only as
  strong as the helper (notary, proxy or enclave) the reader trusts.
- TLSNotary is the most open option (Apache-2.0 or MIT, Rust), but it says it is not for
  production and supports TLS 1.2 only.
- The bigger risk is terms of service: many APIs forbid sharing keys and passing their data on.

## Approaches

### 1. MPC-TLS: TLSNotary
- **How.** The Prover (user) and a Verifier jointly act as the TLS client; they "compute additive
  shares" of the pre-master secret, so neither can talk to the server alone
  ([key exchange](https://tlsnotary.org/docs/mpc/key_exchange)). The server sees normal TLS
  ([intro](https://tlsnotary.org/docs/intro)).
- **Who learns the key.** The user writes the request (with the API key) and can redact it; the
  Verifier works "without seeing the data in plain text" ([intro](https://tlsnotary.org/docs/intro)).
  The Verifier never holds the full session keys ([key exchange](https://tlsnotary.org/docs/mpc/key_exchange)).
- **What the proof says.** A Notary signs commitments to the transcript and the server's identity;
  the user then makes presentations that reveal chosen parts. The example verifier prints
  "Successfully verified that the data below came from a session with test-server.io" with
  hidden bytes shown as X ([attestation example, v0.1.0-alpha.15](https://github.com/tlsnotary/tlsn/blob/v0.1.0-alpha.15/crates/examples/attestation/README.md)).
  Server identity rests on the certificate chain to trusted CAs ([intro](https://tlsnotary.org/docs/intro)).
- **What it does not say.** That the Notary was honest: "Verifiers will only accept the signed data
  if they trust the Notary"; several notaries help against prover-notary collusion
  ([intro](https://tlsnotary.org/docs/intro)). The example README says "The verifier must trust
  the Notary not to collude with the Prover" ([README](https://github.com/tlsnotary/tlsn/blob/v0.1.0-alpha.15/crates/examples/attestation/README.md)).
- **Proxy mode.** A lighter mode where the Verifier forwards encrypted packets and later checks a
  ZK proof; it "Assumes attacker cannot MITM the verifier-to-server connection"
  ([blog 2026-04-22](https://tlsnotary.org/blog/2026/04/22/proxy-mode),
  [docs](https://tlsnotary.org/docs/protocol/proxy-mode)). Running the Verifier yourself "is
  sufficient to rule out collusion" ([docs](https://tlsnotary.org/docs/protocol/proxy-mode)).
  "MPC-TLS remains the default". The blog gives no figures and says benchmarks come later.
- **Limits.** "TLSNotary currently supports TLS 1.2"; the FAQ also says "There are no immediate
  plans to support TLS 1.3" ([FAQ](https://tlsnotary.org/docs/faq)). `max_sent_data` and
  `max_recv_data` are required hard caps; "exceeding it aborts the session"
  ([configuration](https://tlsnotary.org/docs/protocol/configuration)).
- **Maturity and licence.** Apache-2.0 or MIT. README: "should not be used in production",
  "Expect bugs and regular major breaking changes" ([repo](https://github.com/tlsnotary/tlsn)).
  Latest tag `v0.1.0-alpha.15`, released 2026-05-21 (GitHub releases API).
- **Cost.** FAQ estimates for MPC-TLS: "~25MB (a fixed cost per one TLSNotary session)", "~10 MB
  per every 1KB of outgoing data", "~40KB per every 1 KB of incoming data"; a 1 KB request and
  100 KB response is "~39 MB of upload data" ([FAQ](https://tlsnotary.org/docs/faq)). No latency figure.

### 2. DECO (Zhang, Maram, Malvai, Goldfeder, Juels; CCS 2020)
- **How.** Three-party handshake: the user and the oracle run 2PC "so that neither of them has the
  entire session key", which stops the user forging ciphertext; then any ZK system proves
  statements about the committed data. DECO also introduced a proxy mode
  ([survey, arXiv 2606.13892, appendix A](https://arxiv.org/pdf/2606.13892)).
- **Claim.** The abstract says DECO is "the first such system that works without trusted hardware
  or server-side modifications" ([arXiv 1909.00938](https://arxiv.org/abs/1909.00938), extended
  version of the CCS'20 paper).
- **Status.** Press reported Chainlink acquired DECO in 2020
  ([Decrypt](https://decrypt.co/40146/chainlink-buys-cornell-university-tech-to-make-oracles-more-private));
  the survey says DECO is "productized as part of the Chainlink Runtime Environment"
  ([arXiv 2606.13892](https://arxiv.org/pdf/2606.13892)). I found no open DECO code.
- **Cost.** The abstract gives no figures. A third-party blog cites 475 MB for a 2 KB payload;
  not checked against the paper here.

### 3. Proxy witness: Reclaim Protocol
- **How.** An "attestor" proxies the user's TLS traffic, sees only ciphertext, and signs a claim;
  the user proves in ZK that a key it holds decrypts the response, without revealing the key,
  because revealing it would expose earlier messages "including credentials"
  ([blog: the zk in zkTLS](https://blog.reclaimprotocol.org/posts/zk-in-zktls),
  [attestor-core README](https://github.com/reclaimprotocol/attestor-core)).
- **Who learns the key.** Docs: the attestor works "without access to session keys", and claims
  hide "login credentials and access tokens" ([docs](https://docs.reclaimprotocol.org/understanding-the-tech)).
  The same page also says the attestor decrypts "only the necessary data portions"; the page does
  not reconcile the two.
- **Trust.** The proxy must not be bribed to mislabel which side sent which bytes
  ([blog](https://blog.reclaimprotocol.org/posts/zk-in-zktls)). Proxy mode is "vulnerable to
  network-layer attacks (e.g., BGP hijacking)"; "For high-stakes applications, 2PC mode should be
  preferred" ([survey](https://arxiv.org/pdf/2606.13892)). Theory: proxying is secure for HTTPS
  thanks to HTTP's variable padding, and saves "60--90% in running time"; for fixed responses
  ChaCha20-Poly1305 is safe and AES-GCM is not ([ePrint 2024/733](https://eprint.iacr.org/2024/733), AFT 2025).
- **Newer design.** A 2026 paper presents Reclaim built on a three-party "Distributed-AEAD" and
  reports a 1200 kB response attested in 7.28 s on a lower-mid-range phone
  ([ePrint 2026/2187](https://eprint.iacr.org/2026/2187)). The survey says Reclaim is
  "expanding to a new TEE-based protocol" ([arXiv 2606.13892](https://arxiv.org/pdf/2606.13892)).
- **Licence.** `attestor-core` is AGPL-3.0 (GitHub API). TLS version support: not stated.

### 4. Opacity Network
- **How.** Builds on MPC-TLS (TLSNotary); its spec says "The client and the MPC node can collude
  to reconstruct the shared secret" and that "it's impossible to solve the collusion problem
  directly", so it picks a random node after the claim is committed and logs failed attempts
  ([AVS spec](https://hackmd.io/SpNwBquVReqrVJzloT_esA)). Nodes need Intel SGX and are slashed
  for faults such as leaking a key share before finalization
  ([slashing draft](https://hackmd.io/ZG6-EbI_QJeMBBQBv8bUkQ),
  [node README](https://github.com/OpacityLabs/opacity-avs-node)); operators are whitelisted via
  an intake form ([same README](https://github.com/OpacityLabs/opacity-avs-node)).
- **Could not fetch.** docs.opacity.network returned Cloudflare error 1000; opacity.network
  redirects to opacitylabs.com, which now describes investor-credential products, not zkTLS.
  The HackMD drafts are undated. Treat the design as unconfirmed for today.

### 5. zkPass
- **How.** "Proxy Mode" is the default ("P communicates with S through V as a proxy"); "MPC Mode"
  is the fallback for servers that block requests from different IPs
  ([technical overview](https://docs.zkpass.org/overview/technical-overview)). In proxy mode V
  reveals its key share and "P computes the full session key"; in MPC mode V holds part of the MAC
  key and "has no access to the enc_key" (same page).
- **Trust.** Proxy mode "introduces an additional trust assumption for the proxy"; MPC mode
  "still relies on a 'trusted notary'" (same page).
- **Cost.** MPC mode: "a fixed cost of 20MB, plus 8MB per 1KB of outgoing data"; about 32 MB
  upload for 1 KB out and 100 KB in (same page). Proof generation via VOLE-in-the-Head is
  "millisecond-level" per the [introduction](https://docs.zkpass.org/overview/introduction); no
  benchmark linked. Licence and TLS versions: not stated.

### 6. TEE notary: Pluto web proofs
- **How.** The client's TLS goes through a notary in a TEE (Intel TDX; Google Confidential Space
  for prototyping); "only the client and the confidential TEE have access to these secrets"
  ([Pluto blog](https://pluto.xyz/blog/web-proof-techniques-tee-mode)).
- **Trust.** The hardware maker and side-channel resistance (same blog). The survey calls "zkTLS"
  "a misnomer for TEE-based solutions" since they use no ZK proofs ([arXiv 2606.13892](https://arxiv.org/pdf/2606.13892)).
- **Status.** `pluto/web-prover`, Apache-2.0; "As of February 2025 ... operational on Ethereum
  testnets"; last push 2025-03-18 ([repo](https://github.com/pluto/web-prover)). Looks inactive.

## A simpler non-zk baseline
- **User fetches and signs (Onyx proposal).** The user runs a small Onyx client that calls the API
  with their own key and signs the response with their Onyx key. Onyx never sees the key. Nothing
  proves the bytes came from the API, so this is level 4 (a claim) per
  [proof-ladder.md](../spec/proof-ladder.md). It becomes level 3 only if the API itself signs its
  responses (`SignedSource`).
- **Encrypted key stored on Onyx servers.** Onyx would decrypt and use the key, so Onyx sees it.
  Rejected: one breach leaks every user's key, and it is the key sharing the terms below forbid.

## Terms-of-service risk
Hiding the key from Onyx does not make sharing the API's data allowed. Quoted text was checked
against the raw page for Google, GitHub and CoinGecko; Etherscan's page refused direct download
(403), so its quote comes from the fetch tool only.
- Google APIs Terms, section 4(b): "You will keep your credentials confidential and make reasonable
  efforts to prevent and discourage other API Clients from using your credentials." Section 5(e)
  forbids, unless allowed by the owner or law, to "Copy, translate, modify, create a derivative
  work of, sell, lease, lend, convey, distribute, publicly display, or sublicense to any third
  party" ([terms](https://developers.google.com/terms)).
- GitHub Terms, section H: "You may not share API tokens to exceed GitHub's rate limitations."
  ([terms](https://docs.github.com/en/site-policy/github-terms/github-terms-of-service)).
- CoinGecko API Terms: "you are not permitted to sell, rent, lease, sub-license, re-distribute or
  syndicate access to the CoinGecko API or part thereof (unless pursuant to the terms of an
  Executed Agreement that you enter into with CoinGecko)" ([terms](https://www.coingecko.com/en/api_terms)).
- Etherscan API Terms, prohibited activities: "Share, disclose or expose your API keys to any third
  party" and "Operate an API proxy, gateway, relay or data feed that passes API Data through to
  third parties" ([terms](https://etherscan.io/apiterms)).
- Lesson: a notarised keyed pull is still a redistribution. Onyx should accept it only where the
  user owns the data's rights or the API's terms allow onward sharing (Onyx proposal).

## Trust and cost table
| Approach | Who could learn the key | Trust needed | Cost stated by source | Licence / maturity |
|---|---|---|---|---|
| TLSNotary MPC-TLS | No one but the user | Notary does not collude with user; CA system | ~25 MB fixed + ~10 MB/KB sent + ~40 KB/KB received ([FAQ](https://tlsnotary.org/docs/faq)) | Apache-2.0/MIT; alpha, "not ... production" |
| TLSNotary proxy mode | No one but the user | No MITM on verifier-to-server path; verifier does not collude | None yet ([blog](https://tlsnotary.org/blog/2026/04/22/proxy-mode)) | Same repo |
| DECO | No one but the user | Oracle does not collude | None in abstract | Paper; productized in Chainlink, no open code found |
| Reclaim | No one but the user | Attestor honest; no network MITM | 1200 kB in 7.28 s on a phone ([ePrint 2026/2187](https://eprint.iacr.org/2026/2187)) | AGPL-3.0 attestor-core |
| Opacity | No one but the user | SGX, random node, slashing | None found | Docs unreachable |
| zkPass | No one but the user | Proxy or "trusted notary" | 20 MB + 8 MB/KB out (MPC mode) | Licence not stated |
| Pluto TEE | The TEE | Hardware vendor | None | Apache-2.0; inactive since 2025-03 |
| User signs own fetch | No one but the user | The user | None | Onyx level 4 |
| Key on Onyx server | Onyx | Onyx | None | Rejected |

## Recommendation for Onyx
1. Never store users' API keys on Onyx servers, encrypted or not.
2. A notarised response is level 3 ("source-proven") only when no single party can forge it:
   the verifier is the reader itself, or k-of-n independent notaries (Onyx proposal: Onyx's own
   bonded observers as notaries, each signing, with the level-2 majority rule).
3. A response notarised by one third-party notary (Reclaim attestor, a hosted TLSNotary notary,
   one TEE) is level 4 with the attestation kept as evidence: it binds the server name, but the
   reader still trusts one party.
4. Accept only TLS sessions to the host named in the source's registry entry, and store the
   presentation next to the value so anyone can re-verify the signature.
5. Gate it on rights: the submission must state that the user may share the data; reject sources
   whose terms forbid redistribution (see above).

## Smallest next experiment (about one loop, local only, no sign-ups)
Repo `tlsnotary/tlsn`, tag `v0.1.0-alpha.15`, Apache-2.0 or MIT. Goal: a presentation of a real
public HTTPS response, verified in Rust, with one request header hidden.
1. Clone the tag into a scratch directory with its own `CARGO_TARGET_DIR`; build under
   `systemd-run --user --scope -p MemoryMax=...` (TLSNotary MPC is heavy).
2. Run the three attestation examples unchanged against the local fixture
   (`tlsn-server-fixture`, `attestation_prove`, `attestation_present`, `attestation_verify`).
3. Copy `crates/examples/attestation/prove.rs` and change three things: `SERVER_DOMAIN` to
   `api.fiscaldata.treasury.gov`; the root store from the fixture CA to the public web roots; the
   hardcoded Chrome `USER_AGENT` to `onyx-dev`. Request
   `/services/api/fiscal_service/v2/accounting/od/debt_to_penny?sort=-record_date&page[size]=1`
   (returned 200, 1791 bytes over TLS 1.2 with ECDHE-RSA-AES128-GCM-SHA256 when checked today
   with curl and openssl). Add a dummy header `X-Onyx-Test: not-a-real-key` as a stand-in key.
4. Present with that header redacted and the body revealed; verify. Pass: the verifier prints the
   server name `api.fiscaldata.treasury.gov`, the body matches a plain `curl` of the same URL,
   and the dummy header shows as X.
5. Record wall time and bytes moved for each step in this file.
The notary is local, so this proves the mechanics only, not level 3.

## Result of the experiment (2026-10-21 run, recorded 2026-10-24; ROADMAP S2b, partner tracker T11)
Done in `experiments/tlsn-fiscaldata/` (its own workspace; README there). A real presentation of
`api.fiscaldata.treasury.gov` over TLS 1.2 (tlsn `v0.1.0-alpha.15`), request target the pinned
Debt to the Penny record of 2026-10-08, with the 32-byte value of an `X-Demo-Key` header hidden.
The verifier shows the server name, the request with that value as X, and the body; the value
`40305316210829.72` equals the adapter's recorded fetch. One run on the dev box, notary in the
prover's process (`fixtures/summary.json`): 1.34 s in all, 18,553-byte presentation, 30.5 MB
prover-to-notary and 4.0 MB notary-to-prover traffic. Mechanics proved; not level 3, because the
notary was local and is trusted.

## FRED: terms and the first stack to try
FRED is BLOCKED in [data-sources.md](../ops/data-sources.md) row N10. Quotes came through the
fetch tool on 2026-10-20; curl of fred.stlouisfed.org failed, so they are not raw-HTML checked.

**(a) Keys and accounts.** The [FRED legal page](https://fred.stlouisfed.org/legal/), account
security requirements, forbids to "Share your password, let anyone else access your FRED®
account, or do anything else that might jeopardize the security of your FRED® account;". The
[API terms](https://fred.stlouisfed.org/docs/api/terms_of_use.html): "You may only obtain and use
a key in accordance with these Terms of Use and the FRED® API Documentation." and "If you become
aware of any unauthorized use of your password, your account, or your API key, you agree to
notify the Federal Reserve Bank of St. Louis immediately." No sentence found names sharing an
API key as such; in the zkTLS flow the key is not shared anyway. The data is the problem.

**(b) Redistribution, storing, caching.** [Legal page](https://fred.stlouisfed.org/legal/),
property rights: "Except as expressly provided in these Terms of Use, FRED® Content may not be
modified, duplicated, copied, distributed, framed, reproduced, rebranded, republished, uploaded,
downloaded, scraped, displayed, posted, transmitted on any Internet, Intranet or Extranet site,
nor may the FRED® Content be incorporated in any other database or compilation or sold in any
form or by any means, in whole or in part, without the Bank's prior written permission, [...]".
Prohibitions: "Store, cache, or archive any portion of the FRED® Services or FRED® Content;
provide any stored, cached, or archived portion of the FRED® Services or FRED® Content to any
third party; or incorporate any FRED® Content in any database, compilation, archive, cache, or
other medium." The API prohibitions on the same page say the same for "Use the FRED® API in
connection with storing, caching, or archiving [...]". The separate API terms page, per the
fetch tool, has no storing or caching clause.

**(c) Third-party series.** [Legal page](https://fred.stlouisfed.org/legal/): "BEFORE USING DATA
SERIES OWNED BY THIRD PARTIES FOR ANYTHING OTHER THAN YOUR OWN PERSONAL USE, YOU MUST CONTACT
THE DATA OWNER TO OBTAIN PERMISSION." and "Redistribute any third party's proprietary content,
including any graphs, maps, images, logos, data, or datasets, for commercial use without first
obtaining express written permission from the data provider." [API
terms](https://fred.stlouisfed.org/docs/api/terms_of_use.html): "Data series available through
the FRED® API, may be owned by third parties and subject to copyright restrictions." and "You are
solely responsible for complying with any requirements or restrictions imposed on usage of the
data series by their respective owners."

**What it means for the flow.**
- zkTLS hides the key, not the data. A notarised FRED response that Onyx stores and serves is
  FRED Content "incorporated in [a] database" and given to third parties. So the FRED end state
  stays "held for rights review, not published", with or without a real prover.
- Whether keeping only `responseKeccak256` counts as storing FRED Content is for the rights
  review; not answered here.
- What would change it: the Bank's prior written permission (and, for a copyrighted series, the
  data owner's), recorded in data-sources.md; or Onyx reading the primary public-domain source
  instead: BLS for CPI (row N8), Federal Reserve Board H.15 for rates (row N7).

**Which stack first: TLSNotary.** Licence Apache-2.0 or MIT (Reclaim's attestor-core is AGPL-3.0;
zkPass states none); Rust verifier like the rest of Onyx; Onyx can run its own verifier, so no
third-party service or sign-up. FRED's host fits TLS 1.2, which TLSNotary needs: a handshake-only
test today (openssl, no HTTP request sent) to `api.stlouisfed.org:443` gave TLSv1.2 with
ECDHE-ECDSA-AES256-GCM-SHA384 by default, ECDHE-ECDSA-AES128-GCM-SHA256 when offered, and also
TLSv1.3 (TLS_AES_256_GCM_SHA384); the certificate (DigiCert) lists `DNS:api.stlouisfed.org`.
Browser path: `tlsnotary/tlsn-js` is archived (last release v0.1.0-alpha.12, 2025-06-19; GitHub
API). The live one is [tlsn-extension](https://github.com/tlsnotary/tlsn-extension), a Chrome MV3
extension whose prover is "WASM (`tlsn-wasm`) in an offscreen document", Apache-2.0 or MIT,
release 0.1.0.1501 (2026-06-25); its README lists websockify "For WebSocket proxying of TLS
connections (optional)"; the core repo has `crates/wasm` at v0.1.0-alpha.15. So #/submit needs
that extension or a page on `tlsn-wasm`, plus a WebSocket-to-TCP relay. FRED puts the key in the
query string (`api_key=`), so the prover must redact part of the request line.

**Smallest FRED-flow experiment (no sign-up).** Same as the Treasury experiment above, but put
the stand-in key where FRED puts it: request
`...debt_to_penny?sort=-record_date&page[size]=1&api_key=not-a-real-key` (returned 200, 1791
bytes today; the endpoint ignores the extra parameter). Pass: the verified presentation shows
the host and body, and `api_key=` followed by X only. Then feed that presentation's fields into
the S1 stub's output shape (`responseKeccak256`, `responseBytes`) to check they line up.

**Needs the project owner.**
- The one step that needs an account: a FRED API key, created by the project owner himself, used only on
  his machine, never committed or pasted into a log or prompt. A FRED run must still end "held
  for rights review, not published": Onyx stores and serves nothing from the response.

## Open questions
- Which cipher suites TLSNotary accepts; the configuration page does not list them.
- How many keyed APIs Onyx cares about still allow TLS 1.2; TLS 1.3-only hosts rule out TLSNotary today.
- Can several Onyx bonded observers each act as the TLSNotary verifier for one user session, or
  must each run its own session (n times the ~25 MB cost)?
- The user must be online for every pull. Is that acceptable for a usage or billing feed?
- Is Opacity's design still live? Its docs site did not load.
- Do Reclaim's or zkPass's verifiers have an open, offline verification path in Rust?

## Sources
- TLSNotary: [intro](https://tlsnotary.org/docs/intro), [key exchange](https://tlsnotary.org/docs/mpc/key_exchange),
  [FAQ](https://tlsnotary.org/docs/faq), [proxy mode docs](https://tlsnotary.org/docs/protocol/proxy-mode),
  [proxy mode blog](https://tlsnotary.org/blog/2026/04/22/proxy-mode),
  [configuration](https://tlsnotary.org/docs/protocol/configuration), [repo](https://github.com/tlsnotary/tlsn),
  [attestation example at v0.1.0-alpha.15](https://github.com/tlsnotary/tlsn/blob/v0.1.0-alpha.15/crates/examples/attestation/README.md)
- DECO: [arXiv 1909.00938](https://arxiv.org/abs/1909.00938);
  [Decrypt on the Chainlink acquisition](https://decrypt.co/40146/chainlink-buys-cornell-university-tech-to-make-oracles-more-private)
- Survey: [Crypto x AI, AI x Crypto: A Survey, arXiv 2606.13892](https://arxiv.org/pdf/2606.13892) (appendix A, TLS oracles)
- Reclaim: [docs](https://docs.reclaimprotocol.org/understanding-the-tech),
  [the zk in zkTLS](https://blog.reclaimprotocol.org/posts/zk-in-zktls),
  [attestor-core](https://github.com/reclaimprotocol/attestor-core),
  [ePrint 2026/2187](https://eprint.iacr.org/2026/2187); proxying theory [ePrint 2024/733](https://eprint.iacr.org/2024/733)
- Opacity: [AVS spec](https://hackmd.io/SpNwBquVReqrVJzloT_esA), [slashing draft](https://hackmd.io/ZG6-EbI_QJeMBBQBv8bUkQ),
  [node README](https://github.com/OpacityLabs/opacity-avs-node); not fetched: docs.opacity.network (Cloudflare error 1000)
- zkPass: [technical overview](https://docs.zkpass.org/overview/technical-overview), [introduction](https://docs.zkpass.org/overview/introduction)
- Pluto: [TEE mode blog](https://pluto.xyz/blog/web-proof-techniques-tee-mode), [web-prover](https://github.com/pluto/web-prover)
- Terms: [Google APIs](https://developers.google.com/terms),
  [GitHub](https://docs.github.com/en/site-policy/github-terms/github-terms-of-service),
  [CoinGecko](https://www.coingecko.com/en/api_terms), [Etherscan](https://etherscan.io/apiterms)
- FRED: [legal](https://fred.stlouisfed.org/legal/), [API terms](https://fred.stlouisfed.org/docs/api/terms_of_use.html)
  (fetch tool only; curl failed). TLSNotary browser: [tlsn-extension](https://github.com/tlsnotary/tlsn-extension),
  [tlsn-js (archived)](https://github.com/tlsnotary/tlsn-js)
- Not fetched: OpenAI terms of use (403).
