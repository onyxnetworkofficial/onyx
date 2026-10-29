//! ROADMAP S2b prover: one MPC-TLS session with a local notary (a task in this process, over an
//! in-memory pipe), a GET of the pinned Debt to the Penny URL with a random `X-Demo-Key` header,
//! then a presentation that hides only that header's value (and the server's Set-Cookie values).
//!
//! Writes `<out>/presentation.bin` and `<out>/summary.json`. The demo key and the session
//! secrets never touch disk: the presentation is built in memory and the secrets dropped.
//! The notary's signing key is random per run and dropped too; its public key is in the summary.
//!
//! Usage: `prove [--out DIR]` (default `out`). Network: only api.fiscaldata.treasury.gov:443.

use std::{
    future::IntoFuture,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    task::{Context, Poll},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context as _, Result, anyhow, bail};
use futures::io::{AsyncReadExt as _, AsyncWriteExt as _};
use http_body_util::Empty;
use hyper::{Request, StatusCode, body::Bytes};
use hyper_util::rt::TokioIo;
use k256::ecdsa::SigningKey;
use onyx_tlsn_fiscaldata as x;
use rand::RngCore;
use serde_json::json;
use tokio::io::{AsyncRead, AsyncWrite, DuplexStream, ReadBuf};
use tokio_util::compat::{FuturesAsyncReadCompatExt, TokioAsyncReadCompatExt};

use tlsn::{
    Session,
    attestation::{
        Attestation, AttestationConfig, CryptoProvider,
        presentation::Presentation,
        request::{Request as AttestationRequest, RequestConfig},
        signing::Secp256k1Signer,
    },
    config::{
        prove::ProveConfig, prover::ProverConfig, tls::TlsClientConfig,
        tls_commit::mpc::MpcTlsConfig, verifier::VerifierConfig,
    },
    connection::{CertBinding, ConnectionInfo, HandshakeData, ServerName, TranscriptLength},
    prover::ProverOutput,
    transcript::{ContentType, TranscriptCommitConfig},
    verifier::{VerifierCommitStart, VerifierOutput},
    webpki::RootCertStore,
};
use tlsn_formats::http::{DefaultHttpCommitter, HttpCommit, HttpTranscript};

/// Counts bytes each way on the prover's end of the prover-notary pipe.
struct Counting {
    inner: DuplexStream,
    read: Arc<AtomicU64>,
    written: Arc<AtomicU64>,
}

impl AsyncRead for Counting {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let r = Pin::new(&mut self.inner).poll_read(cx, buf);
        let n = (buf.filled().len() - before) as u64;
        self.read.fetch_add(n, Ordering::Relaxed);
        r
    }
}

impl AsyncWrite for Counting {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let r = Pin::new(&mut self.inner).poll_write(cx, buf);
        if let Poll::Ready(Ok(n)) = &r {
            self.written.fetch_add(*n as u64, Ordering::Relaxed);
        }
        r
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// Whole-session limit.
const TIMEOUT_SECS: u64 = 120;

fn phase(name: &str) {
    eprintln!("[prove] {name}");
}

#[derive(Default, Debug)]
struct Timings {
    mpc_setup_us: u128,
    tls_and_http_us: u128,
    prove_us: u128,
    attestation_us: u128,
    presentation_us: u128,
    total_us: u128,
}

fn random_hex(n: usize) -> String {
    let mut b = vec![0u8; n];
    rand::rng().fill_bytes(&mut b);
    hex::encode(b)
}

fn random_signing_key() -> SigningKey {
    loop {
        let mut b = [0u8; 32];
        rand::rng().fill_bytes(&mut b);
        if let Ok(k) = SigningKey::from_slice(&b) {
            return k;
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut out = std::path::PathBuf::from("out");
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => out = args.next().context("--out needs a directory")?.into(),
            _ => bail!("usage: prove [--out DIR]"),
        }
    }
    std::fs::create_dir_all(&out)?;

    // The stand-in API key: 16 random bytes as 32 hex digits, held in memory only.
    let demo_key = random_hex(16);
    let notary_key = random_signing_key();
    let notary_pub = notary_key.verifying_key().to_sec1_bytes().to_vec();

    let (notary_socket, prover_socket) = tokio::io::duplex(1 << 23);
    let read = Arc::new(AtomicU64::new(0));
    let written = Arc::new(AtomicU64::new(0));
    let prover_socket = Counting {
        inner: prover_socket,
        read: read.clone(),
        written: written.clone(),
    };
    let notary_secret = notary_key.to_bytes();
    let notary_task = tokio::spawn(async move { notary(notary_socket, &notary_secret).await });
    drop(notary_key);

    let started = Instant::now();
    let mut t = Timings::default();
    // A hung session (seen once in five runs: no TCP socket open, no progress) must not hang
    // the caller; every phase prints to stderr so a hang shows where it stopped.
    let (presentation, http_status) = tokio::time::timeout(
        std::time::Duration::from_secs(TIMEOUT_SECS),
        prover(prover_socket, &demo_key, &mut t),
    )
    .await
    .map_err(|_| anyhow!("prover timed out after {TIMEOUT_SECS} s"))??;
    t.total_us = started.elapsed().as_micros();
    tokio::time::timeout(std::time::Duration::from_secs(TIMEOUT_SECS), notary_task)
        .await
        .map_err(|_| anyhow!("notary timed out"))???;

    let bytes = x::encode(&presentation)?;
    // Self-check: the key's characters appear nowhere in the file.
    if bytes
        .windows(demo_key.len())
        .any(|w| w == demo_key.as_bytes())
    {
        bail!("the demo key appears in the presentation bytes");
    }
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let v0 = Instant::now();
    let checked = x::verify_presentation(
        &bytes,
        &x::Policy::fiscaldata(notary_pub.clone(), now, Some(600)),
    )
    .map_err(|e| anyhow!("own presentation does not verify: {e}"))?;
    let verify_us = v0.elapsed().as_micros();
    if checked.redacted_range.len() != demo_key.len() {
        bail!("redacted range length differs from the demo key's");
    }

    let summary = json!({
        "prover": x::PROVER_ID,
        "tlsnVersion": x::TLSN_VERSION,
        "serverName": checked.server_name,
        "tlsVersion": checked.tls_version,
        "connectionTimeUnix": checked.connection_time,
        "requestTarget": checked.request_target,
        "userAgent": x::USER_AGENT,
        "redactedHeader": x::REDACTED_HEADER,
        "redactedValueBytes": checked.redacted_range.len(),
        "redactedSentRange": [checked.redacted_range.start, checked.redacted_range.end],
        "demoKeyAbsentFromPresentation": true,
        "hiddenResponseBytes": checked.hidden_response_bytes,
        "httpStatus": http_status,
        "responseBytes": checked.body.len(),
        "responseKeccak256": checked.body_keccak256,
        "jsonPointer": x::JSON_POINTER,
        "value": checked.value,
        "recordDate": checked.record_date,
        "notaryKeyAlg": "secp256k1",
        "notaryPublicKey": hex::encode(&notary_pub),
        "notaryNote": "local notary in the prover's own process; its signing key was random and dropped after the run",
        "presentationBytes": bytes.len(),
        "presentationKeccak256": x::keccak_hex(&bytes),
        "maxSentData": x::MAX_SENT_DATA,
        "maxRecvData": x::MAX_RECV_DATA,
        "proverToNotaryBytes": written.load(Ordering::Relaxed),
        "notaryToProverBytes": read.load(Ordering::Relaxed),
        "timingsUs": {
            "mpcSetup": t.mpc_setup_us,
            "tlsAndHttp": t.tls_and_http_us,
            "prove": t.prove_us,
            "attestation": t.attestation_us,
            "presentation": t.presentation_us,
            "total": t.total_us,
            "verify": verify_us,
        },
    });
    std::fs::write(out.join("presentation.bin"), &bytes)?;
    std::fs::write(
        out.join("summary.json"),
        serde_json::to_string_pretty(&summary)? + "\n",
    )?;
    println!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(())
}

async fn prover(socket: Counting, demo_key: &str, t: &mut Timings) -> Result<(Presentation, u16)> {
    let t0 = Instant::now();
    phase("mpc setup");
    let session = Session::new(socket.compat());
    let (driver, mut handle) = session.split();
    let driver_task = tokio::spawn(driver);

    let prover = handle
        .new_prover(ProverConfig::builder().build()?)?
        .commit(
            MpcTlsConfig::builder()
                .max_sent_data(x::MAX_SENT_DATA)
                .max_recv_data(x::MAX_RECV_DATA)
                .build()?,
        )
        .await?;
    t.mpc_setup_us = t0.elapsed().as_micros();

    let t1 = Instant::now();
    phase("tls connect + request");
    let client_socket = tokio::net::TcpStream::connect((x::SERVER_DOMAIN, 443)).await?;
    let (tls_connection, prover) = prover.connect(
        TlsClientConfig::builder()
            .server_name(ServerName::Dns(x::SERVER_DOMAIN.try_into()?))
            .root_store(RootCertStore::mozilla())
            .build()?,
        client_socket.compat(),
    )?;
    let tls_connection = TokioIo::new(tls_connection.compat());
    let prover_task = tokio::spawn(prover.into_future());
    let (mut request_sender, connection) =
        hyper::client::conn::http1::handshake(tls_connection).await?;
    tokio::spawn(connection);

    let request = Request::builder()
        .uri(x::REQUEST_TARGET)
        .header("Host", x::SERVER_DOMAIN)
        .header("Accept", "application/json")
        // TLSNotary tooling does not support compression.
        .header("Accept-Encoding", "identity")
        .header("Connection", "close")
        .header("User-Agent", x::USER_AGENT)
        .header(x::REDACTED_HEADER, demo_key)
        .body(Empty::<Bytes>::new())?;
    let response = request_sender.send_request(request).await?;
    let status = response.status();
    if status != StatusCode::OK {
        bail!("server answered {status}");
    }
    phase("response received; waiting for TLS close");
    let mut prover = prover_task.await??;
    t.tls_and_http_us = t1.elapsed().as_micros();

    let t2 = Instant::now();
    phase("prove (commitments)");
    let transcript = HttpTranscript::parse(prover.transcript())?;
    let mut builder = TranscriptCommitConfig::builder(prover.transcript());
    DefaultHttpCommitter::default().commit_transcript(&mut builder, &transcript)?;
    let transcript_commit = builder.build()?;
    let mut builder = RequestConfig::builder();
    builder.transcript_commit(transcript_commit);
    let request_config = builder.build()?;
    let mut builder = ProveConfig::builder(prover.transcript());
    if let Some(config) = request_config.transcript_commit() {
        builder.transcript_commit(config.clone());
    }
    let disclosure_config = builder.build()?;
    let ProverOutput {
        transcript_commitments,
        transcript_secrets,
        ..
    } = prover.prove(&disclosure_config).await?;
    let prover_transcript = prover.transcript().clone();
    let tls_transcript = prover.tls_transcript().clone();
    prover.close().await?;
    t.prove_us = t2.elapsed().as_micros();

    let t3 = Instant::now();
    phase("attestation");
    let mut builder = AttestationRequest::builder(&request_config);
    builder
        .server_name(ServerName::Dns(x::SERVER_DOMAIN.try_into()?))
        .handshake_data(HandshakeData {
            certs: tls_transcript
                .server_cert_chain()
                .context("server cert chain")?
                .to_vec(),
            sig: tls_transcript
                .server_signature()
                .context("server signature")?
                .clone(),
            binding: tls_transcript.certificate_binding().clone(),
        })
        .transcript(prover_transcript)
        .transcript_commitments(transcript_secrets, transcript_commitments);
    let (request, secrets) = builder.build(&CryptoProvider::default())?;

    handle.close();
    let mut socket = driver_task.await??;
    socket.write_all(&bincode::serialize(&request)?).await?;
    socket.close().await?;
    let mut attestation_bytes = Vec::new();
    socket.read_to_end(&mut attestation_bytes).await?;
    let attestation: Attestation = bincode::deserialize(&attestation_bytes)?;
    request.validate(&attestation, &CryptoProvider::default())?;
    t.attestation_us = t3.elapsed().as_micros();

    // Presentation: reveal everything except the demo key's value and Set-Cookie values.
    let t4 = Instant::now();
    phase("presentation");
    let transcript = HttpTranscript::parse(secrets.transcript())?;
    let mut builder = secrets.transcript_proof_builder();
    let req = &transcript.requests[0];
    builder.reveal_sent(req.without_data())?;
    builder.reveal_sent(&req.request.target)?;
    for header in &req.headers {
        if header
            .name
            .as_str()
            .eq_ignore_ascii_case(x::REDACTED_HEADER)
        {
            builder.reveal_sent(header.without_value())?;
        } else {
            builder.reveal_sent(header)?;
        }
    }
    let resp = &transcript.responses[0];
    builder.reveal_recv(resp.without_data())?;
    for header in &resp.headers {
        let name = header.name.as_str().to_ascii_lowercase();
        if x::HIDDEN_RESPONSE_HEADERS.contains(&name.as_str()) {
            builder.reveal_recv(header.without_value())?;
        } else {
            builder.reveal_recv(header)?;
        }
    }
    let body = resp.body.as_ref().context("response has no body")?;
    builder.reveal_recv(body)?;
    let transcript_proof = builder.build()?;
    let provider = CryptoProvider::default();
    let mut builder = attestation.presentation_builder(&provider);
    builder
        .identity_proof(secrets.identity_proof())
        .transcript_proof(transcript_proof);
    let presentation = builder.build()?;
    t.presentation_us = t4.elapsed().as_micros();
    Ok((presentation, status.as_u16()))
}

/// The local notary: an MPC-TLS verifier that signs an attestation with `signing_key`.
async fn notary(socket: DuplexStream, signing_key: &[u8]) -> Result<()> {
    let session = Session::new(socket.compat());
    let (driver, mut handle) = session.split();
    let driver_task = tokio::spawn(driver);

    let verifier_config = VerifierConfig::builder()
        .root_store(RootCertStore::mozilla())
        .build()?;
    let verifier = match handle.new_verifier(verifier_config)?.commit().await? {
        VerifierCommitStart::Mpc(verifier) => verifier.accept().await?.run().await?,
        VerifierCommitStart::Proxy(verifier) => {
            verifier.reject(Some("expecting to use MPC-TLS")).await?;
            bail!("protocol configuration rejected");
        }
    };
    let (
        VerifierOutput {
            transcript_commitments,
            ..
        },
        verifier,
    ) = verifier.verify().await?.accept().await?;
    let tls_transcript = verifier.tls_transcript().clone();
    verifier.close().await?;

    let len = |records: &[tlsn::transcript::Record]| -> usize {
        records
            .iter()
            .filter(|r| matches!(r.typ, ContentType::ApplicationData))
            .map(|r| r.ciphertext.len())
            .sum()
    };
    let sent_len = len(tls_transcript.sent());
    let recv_len = len(tls_transcript.recv());

    handle.close();
    let mut socket = driver_task.await??;
    let mut request_bytes = Vec::new();
    socket.read_to_end(&mut request_bytes).await?;
    let request: AttestationRequest = bincode::deserialize(&request_bytes)?;

    let mut provider = CryptoProvider::default();
    provider
        .signer
        .set_signer(Box::new(Secp256k1Signer::new(signing_key)?));
    let mut cfg = AttestationConfig::builder();
    cfg.supported_signature_algs(Vec::from_iter(provider.signer.supported_algs()));
    let cfg = cfg.build()?;
    let CertBinding::V1_2(binding) = tls_transcript.certificate_binding() else {
        bail!("unsupported cert binding version");
    };
    let mut builder = Attestation::builder(&cfg).accept_request(request)?;
    builder
        .connection_info(ConnectionInfo {
            time: tls_transcript.time(),
            version: tls_transcript.version(),
            transcript_length: TranscriptLength {
                sent: sent_len as u32,
                received: recv_len as u32,
            },
        })
        .server_ephemeral_key(binding.server_ephemeral_key.clone())
        .transcript_commitments(transcript_commitments);
    let attestation = builder.build(&provider)?;
    socket.write_all(&bincode::serialize(&attestation)?).await?;
    socket.close().await?;
    Ok(())
}
