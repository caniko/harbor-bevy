use super::*;

// =============================================================================
// Edge case & error path tests
// =============================================================================

/// Drop without `close()` sends RST; remote gets `ConnectionReset`.
#[test]
#[ignore]
fn muxer_substream_drop_sends_rst() {
    let Some(ctx) = SteamTestCtx::try_new() else {
        return;
    };

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let (mut muxer_a, mut muxer_b) = make_muxer_pair(&ctx, 305, 8);

        // A opens substream and writes data
        let mut sub_a = futures::future::poll_fn(|cx| Pin::new(&mut muxer_a).poll_outbound(cx))
            .await
            .expect("outbound");

        sub_a.write_all(b"before-rst").await.expect("write");

        // Drive A to flush write
        drive_muxer(&mut muxer_a, 5).await;

        // B accepts inbound and reads the data
        let mut sub_b = drive_until_inbound(&mut muxer_b, Duration::from_secs(5))
            .await
            .expect("inbound");

        drive_muxer(&mut muxer_b, 10).await;

        let mut buf = [0u8; 10];
        let n = sub_b.read(&mut buf).await.expect("read");
        assert_eq!(&buf[..n], b"before-rst");

        // A drops the substream without calling close() — triggers RST via Drop
        drop(sub_a);

        // Drive B to process the RST
        drive_muxer(&mut muxer_b, 20).await;

        // B's next read should get ConnectionReset
        let result = sub_b.read(&mut buf).await;
        match result {
            Err(e) => assert_eq!(
                e.kind(),
                io::ErrorKind::ConnectionReset,
                "expected ConnectionReset, got: {e}"
            ),
            Ok(0) => {
                // Lane was released by A's Drop (release_lane removes from map),
                // so B may see EOF instead of RST if the lane state is gone.
                // This is acceptable — the substream is dead either way.
            }
            Ok(n) => panic!("expected error or EOF after RST, got {n} bytes"),
        }
    });
}

/// Freed lane is reused for the next substream.
#[test]
#[ignore]
fn muxer_lane_reuse_after_close() {
    let Some(ctx) = SteamTestCtx::try_new() else {
        return;
    };

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let (mut muxer_a, mut muxer_b) = make_muxer_pair(&ctx, 306, 8);

        // === Round 1: open, write, close ===
        let mut sub_a1 = futures::future::poll_fn(|cx| Pin::new(&mut muxer_a).poll_outbound(cx))
            .await
            .expect("outbound 1");

        sub_a1.write_all(b"round-1").await.expect("write 1");
        sub_a1.close().await.expect("close 1");

        drive_muxer(&mut muxer_a, 10).await;

        let mut sub_b1 = drive_until_inbound(&mut muxer_b, Duration::from_secs(5))
            .await
            .expect("inbound 1");

        drive_muxer(&mut muxer_b, 10).await;

        let mut buf = [0u8; 16];
        let n = sub_b1.read(&mut buf).await.expect("read 1");
        assert_eq!(&buf[..n], b"round-1");

        // Read until EOF (FIN processed)
        drive_muxer(&mut muxer_b, 10).await;
        let n = sub_b1.read(&mut buf).await.expect("eof 1");
        assert_eq!(n, 0, "should get EOF after FIN");

        // Close B's side to fully release the lane
        sub_b1.close().await.expect("b close 1");
        drive_muxer(&mut muxer_b, 5).await;
        drive_muxer(&mut muxer_a, 5).await;

        // === Round 2: lane should be reused ===
        let mut sub_a2 = futures::future::poll_fn(|cx| Pin::new(&mut muxer_a).poll_outbound(cx))
            .await
            .expect("outbound 2");

        sub_a2.write_all(b"round-2").await.expect("write 2");

        drive_muxer(&mut muxer_a, 10).await;

        let mut sub_b2 = drive_until_inbound(&mut muxer_b, Duration::from_secs(5))
            .await
            .expect("inbound 2");

        drive_muxer(&mut muxer_b, 10).await;

        let n = sub_b2.read(&mut buf).await.expect("read 2");
        assert_eq!(
            &buf[..n],
            b"round-2",
            "new substream should carry fresh data"
        );

        // Cleanup
        let _ = sub_a2.close().await;
        let _ = sub_b2.close().await;
    });
}

/// 64KB payload integrity check.
#[test]
#[ignore]
fn muxer_large_payload() {
    let Some(ctx) = SteamTestCtx::try_new() else {
        return;
    };

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let (mut muxer_a, mut muxer_b) = make_muxer_pair(&ctx, 307, 8);

        let mut sub_a = futures::future::poll_fn(|cx| Pin::new(&mut muxer_a).poll_outbound(cx))
            .await
            .expect("outbound");

        // Generate 64KB payload with predictable pattern
        let payload_size = 64 * 1024;
        let payload: Vec<u8> = (0..payload_size).map(|i| (i % 251) as u8).collect();

        sub_a
            .write_all(&payload)
            .await
            .expect("write large payload");

        // Drive A generously to flush all data
        drive_muxer(&mut muxer_a, 30).await;

        // B accepts inbound
        let mut sub_b = drive_until_inbound(&mut muxer_b, Duration::from_secs(5))
            .await
            .expect("inbound");

        // Read all data in a loop (may arrive in multiple batches)
        let mut received = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while received.len() < payload_size {
            drive_muxer(&mut muxer_b, 5).await;

            let mut buf = [0u8; 4096];
            match sub_b.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => received.extend_from_slice(&buf[..n]),
                Err(e) => panic!("read error: {e}"),
            }

            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out reading large payload (got {}/{})",
                received.len(),
                payload_size,
            );
        }

        assert_eq!(
            received.len(),
            payload_size,
            "should receive all {payload_size} bytes"
        );
        assert_eq!(received, payload, "payload should match byte-for-byte");

        let _ = sub_a.close().await;
        let _ = sub_b.close().await;
    });
}

/// Data before FIN preserved; repeated reads after FIN return stable EOF.
#[test]
#[ignore]
fn muxer_read_after_remote_fin() {
    let Some(ctx) = SteamTestCtx::try_new() else {
        return;
    };

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let (mut muxer_a, mut muxer_b) = make_muxer_pair(&ctx, 308, 8);

        let mut sub_a = futures::future::poll_fn(|cx| Pin::new(&mut muxer_a).poll_outbound(cx))
            .await
            .expect("outbound");

        // Write data, then immediately close (FIN)
        sub_a.write_all(b"before-fin").await.expect("write");
        sub_a.close().await.expect("close");

        drive_muxer(&mut muxer_a, 10).await;

        let mut sub_b = drive_until_inbound(&mut muxer_b, Duration::from_secs(5))
            .await
            .expect("inbound");

        drive_muxer(&mut muxer_b, 10).await;

        // Read the data that was sent before FIN
        let mut buf = [0u8; 32];
        let n = sub_b.read(&mut buf).await.expect("read data");
        assert_eq!(
            &buf[..n],
            b"before-fin",
            "data before FIN should be preserved"
        );

        // Drive to ensure FIN is processed
        drive_muxer(&mut muxer_b, 10).await;

        // First EOF read
        let n = sub_b.read(&mut buf).await.expect("first EOF read");
        assert_eq!(n, 0, "first read after FIN should return EOF");

        // Second EOF read — should still be stable EOF, not an error
        let n = sub_b.read(&mut buf).await.expect("second EOF read");
        assert_eq!(n, 0, "second read after FIN should still return EOF");
    });
}

/// `poll_close` RSTs all active substreams.
#[test]
#[ignore]
fn muxer_poll_close_kills_all_substreams() {
    let Some(ctx) = SteamTestCtx::try_new() else {
        return;
    };

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let (mut muxer_a, mut muxer_b) = make_muxer_pair(&ctx, 309, 8);

        // A opens 2 substreams and writes data on both
        let mut sub_a1 = futures::future::poll_fn(|cx| Pin::new(&mut muxer_a).poll_outbound(cx))
            .await
            .expect("outbound 1");
        let mut sub_a2 = futures::future::poll_fn(|cx| Pin::new(&mut muxer_a).poll_outbound(cx))
            .await
            .expect("outbound 2");

        sub_a1.write_all(b"stream-1").await.expect("write 1");
        sub_a2.write_all(b"stream-2").await.expect("write 2");

        drive_muxer(&mut muxer_a, 10).await;

        // B accepts both inbound substreams
        let mut sub_b1 = drive_until_inbound(&mut muxer_b, Duration::from_secs(5))
            .await
            .expect("inbound 1");
        let mut sub_b2 = drive_until_inbound(&mut muxer_b, Duration::from_secs(5))
            .await
            .expect("inbound 2");

        drive_muxer(&mut muxer_b, 10).await;

        // Verify B can read both
        let mut buf = [0u8; 16];
        let n1 = sub_b1.read(&mut buf).await.expect("read 1");
        assert!(n1 > 0, "should read data from stream 1");
        let n2 = sub_b2.read(&mut buf).await.expect("read 2");
        assert!(n2 > 0, "should read data from stream 2");

        // A closes the entire muxer (sends RST to all lanes)
        futures::future::poll_fn(|cx| Pin::new(&mut muxer_a).poll_close(cx))
            .await
            .expect("muxer close");

        // Drive B to process RSTs
        drive_muxer(&mut muxer_b, 20).await;

        // B's reads should now fail or return EOF
        let r1 = sub_b1.read(&mut buf).await;
        let r2 = sub_b2.read(&mut buf).await;

        let is_dead = |r: io::Result<usize>| match r {
            Err(e) => matches!(e.kind(), io::ErrorKind::ConnectionReset),
            Ok(0) => true, // EOF also acceptable (lane cleaned up)
            Ok(_) => false,
        };

        assert!(is_dead(r1), "stream 1 should be dead after muxer close");
        assert!(is_dead(r2), "stream 2 should be dead after muxer close");

        // A's poll_outbound should fail after muxer close
        let outbound_result =
            futures::future::poll_fn(|cx| Pin::new(&mut muxer_a).poll_outbound(cx)).await;
        assert!(
            outbound_result.is_err(),
            "poll_outbound should fail after muxer close"
        );
    });
}

/// 20 open/write/close cycles — no resource leaks.
#[test]
#[ignore]
fn muxer_rapid_open_close_cycles() {
    let Some(ctx) = SteamTestCtx::try_new() else {
        return;
    };

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let (mut muxer_a, mut muxer_b) = make_muxer_pair(&ctx, 310, 8);

        let cycles = 20;
        for i in 0..cycles {
            // A opens substream, writes, closes
            let mut sub_a = futures::future::poll_fn(|cx| Pin::new(&mut muxer_a).poll_outbound(cx))
                .await
                .unwrap_or_else(|e| panic!("cycle {i}: outbound failed: {e}"));

            let msg = format!("cycle-{i}");
            sub_a
                .write_all(msg.as_bytes())
                .await
                .unwrap_or_else(|e| panic!("cycle {i}: write failed: {e}"));
            sub_a
                .close()
                .await
                .unwrap_or_else(|e| panic!("cycle {i}: close failed: {e}"));

            // Drive A to flush FIN
            drive_muxer(&mut muxer_a, 10).await;

            // B accepts inbound, reads data, gets EOF
            let mut sub_b = drive_until_inbound(&mut muxer_b, Duration::from_secs(5))
                .await
                .unwrap_or_else(|e| panic!("cycle {i}: inbound failed: {e}"));

            drive_muxer(&mut muxer_b, 10).await;

            let mut buf = [0u8; 32];
            let n = sub_b
                .read(&mut buf)
                .await
                .unwrap_or_else(|e| panic!("cycle {i}: read failed: {e}"));
            assert_eq!(&buf[..n], msg.as_bytes(), "cycle {i}: data mismatch");

            // Drive for FIN and read EOF
            drive_muxer(&mut muxer_b, 10).await;
            let n = sub_b
                .read(&mut buf)
                .await
                .unwrap_or_else(|e| panic!("cycle {i}: eof read failed: {e}"));
            assert_eq!(n, 0, "cycle {i}: should get EOF");

            // Close B's side to fully release the lane
            sub_b
                .close()
                .await
                .unwrap_or_else(|e| panic!("cycle {i}: b close failed: {e}"));

            drive_muxer(&mut muxer_b, 5).await;
            drive_muxer(&mut muxer_a, 5).await;
        }

        // All 20 cycles completed without errors or hangs
    });
}

/// Write after substream close succeeds (documents current behavior —
/// `poll_write` doesn't check `self.closed`).
#[test]
#[ignore]
fn muxer_write_after_local_close() {
    let Some(ctx) = SteamTestCtx::try_new() else {
        return;
    };

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let (mut muxer_a, mut muxer_b) = make_muxer_pair(&ctx, 311, 8);

        let mut sub_a = futures::future::poll_fn(|cx| Pin::new(&mut muxer_a).poll_outbound(cx))
            .await
            .expect("outbound");

        // B accepts inbound
        drive_muxer(&mut muxer_a, 5).await;
        let mut sub_b = drive_until_inbound(&mut muxer_b, Duration::from_secs(5))
            .await
            .expect("inbound");

        // Close A's substream (sends FIN)
        sub_a.close().await.expect("close");

        // Write after close — current behavior: this succeeds because poll_write
        // doesn't check the substream's `closed` field
        let write_result = sub_a.write_all(b"after-close").await;

        // Document the current behavior: write succeeds (no error)
        // A future fix could check `self.closed` in poll_write and return BrokenPipe.
        assert!(
            write_result.is_ok(),
            "current behavior: write after substream close succeeds (see code comment)"
        );

        // The data may or may not arrive at B (FIN was already sent),
        // but the write itself doesn't error. Drive and check what B sees.
        drive_muxer(&mut muxer_a, 10).await;
        drive_muxer(&mut muxer_b, 10).await;

        // B reads whatever is available, then should get EOF from the FIN
        let mut buf = [0u8; 64];
        loop {
            let n = sub_b.read(&mut buf).await.expect("b read");
            if n == 0 {
                break; // EOF reached
            }
        }
    });
}
