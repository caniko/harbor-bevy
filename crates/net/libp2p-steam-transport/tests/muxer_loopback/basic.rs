use super::*;

#[test]
#[ignore]
fn muxer_outbound_inbound_roundtrip() {
    let Some(ctx) = SteamTestCtx::try_new() else {
        return;
    };

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let (mut muxer_a, mut muxer_b) = make_muxer_pair(&ctx, 300, 8);

        // A opens an outbound substream
        let mut sub_a = futures::future::poll_fn(|cx| Pin::new(&mut muxer_a).poll_outbound(cx))
            .await
            .expect("poll_outbound should succeed");

        // B accepts the inbound substream (drive + poll in loop)
        let mut sub_b = drive_until_inbound(&mut muxer_b, Duration::from_secs(5))
            .await
            .expect("poll_inbound should succeed");

        // A writes data
        let payload = b"hello from muxer A";
        sub_a
            .write_all(payload)
            .await
            .expect("write should succeed");

        // Drive A's muxer to flush, then drive B to receive
        drive_muxer(&mut muxer_a, 5).await;
        drive_muxer(&mut muxer_b, 10).await;

        // B reads data
        let mut buf = vec![0u8; payload.len()];
        sub_b
            .read_exact(&mut buf)
            .await
            .expect("read should succeed");
        assert_eq!(&buf, payload, "B should receive exactly what A sent");

        // Cleanup
        let _ = sub_a.close().await;
        let _ = sub_b.close().await;
    });
}

/// 4 concurrent substreams A→B, no cross-talk.
#[test]
#[ignore]
fn muxer_multiple_concurrent_substreams() {
    let Some(ctx) = SteamTestCtx::try_new() else {
        return;
    };

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let (mut muxer_a, mut muxer_b) = make_muxer_pair(&ctx, 301, 8);

        let messages: Vec<Vec<u8>> = (0..4)
            .map(|i| format!("message on substream {i}").into_bytes())
            .collect();

        // Open 4 outbound substreams on A
        let mut subs_a = Vec::new();
        for _ in 0..4 {
            let sub = futures::future::poll_fn(|cx| Pin::new(&mut muxer_a).poll_outbound(cx))
                .await
                .expect("poll_outbound should succeed");
            subs_a.push(sub);
        }

        // Write different data on each substream
        for (i, sub) in subs_a.iter_mut().enumerate() {
            sub.write_all(&messages[i])
                .await
                .expect("write should succeed");
        }

        // Drive A to flush writes
        drive_muxer(&mut muxer_a, 10).await;

        // Accept 4 inbound substreams on B (drive + poll in loop) and read data
        let timeout = Duration::from_secs(5);
        for _i in 0..4 {
            let mut sub_b = drive_until_inbound(&mut muxer_b, timeout)
                .await
                .expect("poll_inbound should succeed");

            // Drive B to ensure data is available
            drive_muxer(&mut muxer_b, 10).await;

            let mut buf = vec![0u8; 32];
            let n = sub_b.read(&mut buf).await.expect("read should succeed");
            buf.truncate(n);
            // Note: lane assignment order may differ, so we check that the
            // received message matches one of the sent messages.
            assert!(
                messages.contains(&buf),
                "received data should match one of the sent messages, got: {:?}",
                String::from_utf8_lossy(&buf)
            );
        }
    });
}

/// 9th substream triggers lane pool growth.
#[test]
#[ignore]
fn muxer_lane_pool_growth() {
    let Some(ctx) = SteamTestCtx::try_new() else {
        return;
    };

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        // 16 initial lanes → 8 even (for initiator). Opening 9 forces growth.
        let (mut muxer_a, mut _muxer_b) = make_muxer_pair(&ctx, 302, 16);

        // Open 9 substreams (initiator has 8 even lanes, 9th forces grow)
        let mut subs = Vec::new();
        for i in 0..9 {
            let sub = futures::future::poll_fn(|cx| Pin::new(&mut muxer_a).poll_outbound(cx))
                .await
                .unwrap_or_else(|e| panic!("substream {i} should open: {e}"));
            subs.push(sub);
        }

        assert_eq!(subs.len(), 9, "all 9 substreams should be open");

        // Cleanup
        for sub in &mut subs {
            let _ = sub.close().await;
        }
    });
}

/// Simultaneous outbound substreams in both directions.
#[test]
#[ignore]
fn muxer_bidirectional_substreams() {
    let Some(ctx) = SteamTestCtx::try_new() else {
        return;
    };

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let (mut muxer_a, mut muxer_b) = make_muxer_pair(&ctx, 303, 8);

        // A opens outbound
        let mut sub_a_out = futures::future::poll_fn(|cx| Pin::new(&mut muxer_a).poll_outbound(cx))
            .await
            .expect("A outbound should succeed");

        // B opens outbound
        let mut sub_b_out = futures::future::poll_fn(|cx| Pin::new(&mut muxer_b).poll_outbound(cx))
            .await
            .expect("B outbound should succeed");

        // Write in both directions
        let msg_a = b"from A to B";
        let msg_b = b"from B to A";
        sub_a_out.write_all(msg_a).await.expect("A write");
        sub_b_out.write_all(msg_b).await.expect("B write");

        // Drive both to deliver messages and process OPEN signals
        drive_muxer(&mut muxer_a, 10).await;
        drive_muxer(&mut muxer_b, 10).await;

        // Accept inbound on both sides using non-blocking poll loop
        let timeout = Duration::from_secs(5);
        let mut sub_b_in = drive_until_inbound(&mut muxer_b, timeout)
            .await
            .expect("B inbound should succeed");
        let mut sub_a_in = drive_until_inbound(&mut muxer_a, timeout)
            .await
            .expect("A inbound should succeed");

        // Drive to ensure data is delivered
        drive_muxer(&mut muxer_a, 5).await;
        drive_muxer(&mut muxer_b, 5).await;

        // Read and verify
        let mut buf_b = vec![0u8; msg_a.len()];
        sub_b_in
            .read_exact(&mut buf_b)
            .await
            .expect("B read from A");
        assert_eq!(&buf_b, msg_a);

        let mut buf_a = vec![0u8; msg_b.len()];
        sub_a_in
            .read_exact(&mut buf_a)
            .await
            .expect("A read from B");
        assert_eq!(&buf_a, msg_b);
    });
}

/// A writes + closes (FIN), B reads data then EOF.
#[test]
#[ignore]
fn muxer_substream_graceful_close() {
    let Some(ctx) = SteamTestCtx::try_new() else {
        return;
    };

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let (mut muxer_a, mut muxer_b) = make_muxer_pair(&ctx, 304, 8);

        // A opens substream, writes, then closes
        let mut sub_a = futures::future::poll_fn(|cx| Pin::new(&mut muxer_a).poll_outbound(cx))
            .await
            .expect("outbound");

        let payload = b"final message before close";
        sub_a.write_all(payload).await.expect("write");
        sub_a.close().await.expect("close");

        // Drive A to flush
        drive_muxer(&mut muxer_a, 10).await;

        // B accepts inbound (drive + poll in loop)
        let mut sub_b = drive_until_inbound(&mut muxer_b, Duration::from_secs(5))
            .await
            .expect("inbound");

        let mut buf = vec![0u8; payload.len()];
        sub_b.read_exact(&mut buf).await.expect("read data");
        assert_eq!(&buf, payload);

        // Drive B to process FIN
        drive_muxer(&mut muxer_b, 10).await;

        // Next read should return EOF (0 bytes)
        let mut eof_buf = [0u8; 1];
        let n = sub_b.read(&mut eof_buf).await.expect("read after close");
        assert_eq!(n, 0, "should get EOF after remote close");
    });
}
