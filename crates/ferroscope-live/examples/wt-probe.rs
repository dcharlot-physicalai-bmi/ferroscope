//! A WebTransport client that saves the stream to a file — the CI proof that the stream is the
//! file, over QUIC. Certificate validation is disabled because the peer is a localhost producer
//! with a fresh self-signed identity; a browser holds the same trust via the printed hash.

use wtransport::{ClientConfig, Endpoint};

fn main() {
    let mut args = std::env::args().skip(1);
    let url = args.next().expect("usage: wt-probe <url> <out>");
    let out = args.next().expect("usage: wt-probe <url> <out>");

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async move {
        let config = ClientConfig::builder()
            .with_bind_default()
            .with_no_cert_validation()
            .build();
        let conn = Endpoint::client(config)
            .unwrap()
            .connect(&url)
            .await
            .expect("connect");
        let mut stream = conn
            .accept_uni()
            .await
            .expect("the server opens one unidirectional stream per viewer");
        let mut data = Vec::new();
        let mut buf = vec![0u8; 64 * 1024];
        let started = std::time::Instant::now();
        loop {
            match stream.read(&mut buf).await {
                Ok(Some(n)) => data.extend_from_slice(&buf[..n]),
                Ok(None) => break, // FIN: the recording sealed, and this is exactly the file
                Err(e) => {
                    // Say what DID arrive before the failure: "died at byte 0" and "died at the
                    // last record" are different bugs wearing the same error.
                    eprintln!(
                        "read failed after {} bytes at t={:?}: {e}",
                        data.len(),
                        started.elapsed()
                    );
                    std::process::exit(3);
                }
            }
        }
        std::fs::write(&out, &data).expect("write");
        println!("{} bytes", data.len());
    });
}
